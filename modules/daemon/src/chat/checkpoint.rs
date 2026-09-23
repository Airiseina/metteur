//! Paired file/model checkpoints. Never reconstruct model context by truncating
//! display messages: summaries and tool resources can contain later-turn data.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::session::{ChatSessionRecord, load_thread};
use crate::error::{DaemonError, DaemonResult};
use crate::storage::persistence::{Db, cf};
use crate::storage::versioning::VersionManager;

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    snapshot_id: Uuid,
    created_at: u64,
    before: ChatSessionRecord,
}

/// Called under the workspace admission gate, before the agent can mutate files.
pub fn capture(
    db: &Db,
    versions: &VersionManager,
    before: ChatSessionRecord,
    description: &str,
) -> DaemonResult<Uuid> {
    let snapshot = versions.create_snapshot(description)?;
    let checkpoint = Checkpoint {
        snapshot_id: snapshot.id,
        created_at: snapshot.created_at,
        before,
    };
    let encoded =
        serde_json::to_vec(&checkpoint).map_err(|e| DaemonError::Serialization(e.to_string()))?;
    db.put(cf::CHAT_CHECKPOINTS, snapshot.id.as_bytes(), &encoded)?;
    // Context copies are bounded independently of the lightweight file manifests.
    let mut own = checkpoints(db)?
        .into_iter()
        .filter(|(_, c)| c.before.session_id == checkpoint.before.session_id)
        .collect::<Vec<_>>();
    own.sort_by_key(|(_, c)| c.created_at);
    for (key, _) in own.iter().take(own.len().saturating_sub(64)) {
        db.delete(cf::CHAT_CHECKPOINTS, key)?;
    }
    Ok(snapshot.id)
}

fn checkpoints(db: &Db) -> DaemonResult<Vec<(Vec<u8>, Checkpoint)>> {
    db.scan(cf::CHAT_CHECKPOINTS)?
        .into_iter()
        .map(|(key, value)| {
            let record = serde_json::from_slice(&value)
                .map_err(|e| DaemonError::Serialization(e.to_string()))?;
            Ok((key, record))
        })
        .collect()
}

/// No legacy fallback: a file-only snapshot cannot reproduce a compressed context.
pub fn validate(db: &Db, session_id: Uuid, snapshot_id: Uuid) -> DaemonResult<()> {
    let checkpoint = load(db, session_id, snapshot_id)?;
    let current = load_thread(db, &session_id)?
        .ok_or_else(|| DaemonError::NotFound("chat session".into()))?;
    if !current
        .transcript
        .iter()
        .any(|entry| entry.checkpoint == checkpoint.snapshot_id.to_string())
    {
        return Err(DaemonError::Execution(
            "checkpoint is not on the current conversation branch".into(),
        ));
    }
    Ok(())
}

fn load(db: &Db, session_id: Uuid, snapshot_id: Uuid) -> DaemonResult<Checkpoint> {
    let data = db.get(cf::CHAT_CHECKPOINTS, snapshot_id.as_bytes())?.ok_or_else(|| {
        DaemonError::NotFound(
            "paired chat checkpoint; older file-only snapshots cannot rewind conversation safely"
                .into(),
        )
    })?;
    let checkpoint: Checkpoint =
        serde_json::from_slice(&data).map_err(|e| DaemonError::Serialization(e.to_string()))?;
    if checkpoint.before.session_id != session_id || checkpoint.snapshot_id != snapshot_id {
        return Err(DaemonError::PermissionDenied(
            "checkpoint belongs to another conversation".into(),
        ));
    }
    Ok(checkpoint)
}

pub fn check_owner(db: &Db, session_id: Uuid, snapshot_id: Uuid) -> DaemonResult<()> {
    load(db, session_id, snapshot_id).map(|_| ())
}

/// Caller must hold the workspace gate and wait for all agent writes to stop.
/// File-system writes cannot share a RocksDB transaction; take a recovery snapshot
/// and compensate only applied file changes if restore or DB commit fails.
pub fn restore(
    db: &Db,
    versions: &VersionManager,
    session_id: Uuid,
    snapshot_id: Uuid,
) -> DaemonResult<ChatSessionRecord> {
    validate(db, session_id, snapshot_id)?;
    let mut before = load(db, session_id, snapshot_id)?.before;
    before.updated_at = chrono::Utc::now().timestamp_millis() as u64;
    let obsolete = checkpoints(db)?
        .into_iter()
        .filter(|(_, checkpoint)| {
            checkpoint.before.session_id == session_id
                && !before
                    .transcript
                    .iter()
                    .any(|entry| entry.checkpoint == checkpoint.snapshot_id.to_string())
        })
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
    let encoded =
        serde_json::to_vec(&before).map_err(|e| DaemonError::Serialization(e.to_string()))?;
    versions.rollback_with_commit(snapshot_id, || {
        db.commit_chat_rewind(session_id.as_bytes(), &encoded, &obsolete)
    })?;
    Ok(before)
}

pub fn delete_for_session(db: &Db, session_id: Uuid) -> DaemonResult<()> {
    for (key, checkpoint) in checkpoints(db)? {
        if checkpoint.before.session_id == session_id {
            db.delete(cf::CHAT_CHECKPOINTS, &key)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::session::save_thread;
    use crate::chat::transcript::Transcript;
    use metteur_shared::llm::{ContextManager, Message, Role};

    fn setup() -> (std::path::PathBuf, Db, VersionManager, ChatSessionRecord) {
        let root = std::env::temp_dir().join(format!("metteur-chat-rewind-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("code.txt"), "before").unwrap();
        let db = Db::open(&root.join(".metteur/db")).unwrap();
        let versions = VersionManager::new(db.clone(), root.clone());
        let mut context = ContextManager::default();
        context.push_message(Message::text(Role::User, "keep this earlier turn"));
        let mut before = ChatSessionRecord::new(1, context);
        let mut transcript = Transcript::default();
        transcript.push_user("keep this earlier turn", 1);
        before.transcript = transcript.finish();
        before.turns = 1;
        before.todos = serde_json::from_value(serde_json::json!([
            {"content":"earlier plan","status":"pending","active_form":"Planning"}
        ]))
        .unwrap();
        (root, db, versions, before)
    }

    fn save_changed(db: &Db, before: &ChatSessionRecord, checkpoint: Uuid) {
        let mut current = before.clone();
        // A compressed summary can contain discarded content even when individual
        // user/tool messages have disappeared. Restore must replace the context.
        current.context.messages.clear();
        current
            .context
            .push_message(Message::text(Role::Assistant, "summary contains discarded secret"));
        current.todos.clear();
        current.turns += 1;
        let mut transcript = Transcript::resuming(before.transcript.clone());
        transcript.push_user("discard this turn", 2);
        transcript.set_user_checkpoint(&checkpoint.to_string());
        transcript.push_assistant("discard this answer", "discard thinking", 3);
        current.transcript = transcript.finish();
        save_thread(db, &current).unwrap();
    }

    #[test]
    fn rewind_restores_files_full_context_transcript_and_todos_across_restart() {
        let (root, db, versions, before) = setup();
        let checkpoint = capture(&db, &versions, before.clone(), "before turn").unwrap();
        save_changed(&db, &before, checkpoint);
        std::fs::write(root.join("code.txt"), "after").unwrap();
        std::fs::write(root.join("new.txt"), "created by discarded turn").unwrap();
        let restored = restore(&db, &versions, before.session_id, checkpoint).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("code.txt")).unwrap(), "before");
        assert!(!root.join("new.txt").exists());
        assert_eq!(
            serde_json::to_value(&restored.context).unwrap(),
            serde_json::to_value(&before.context).unwrap()
        );
        assert_eq!(restored.todos, before.todos);
        assert_eq!(restored.turns, before.turns);
        assert_eq!(
            serde_json::to_value(&restored.transcript).unwrap(),
            serde_json::to_value(&before.transcript).unwrap()
        );
        drop(versions);
        drop(db);
        let reopened = Db::open(&root.join(".metteur/db")).unwrap();
        let persisted = load_thread(&reopened, &before.session_id).unwrap().unwrap();
        assert!(!serde_json::to_string(&persisted).unwrap().contains("discard"));
        assert!(
            restore(
                &reopened,
                &VersionManager::new(reopened.clone(), root),
                before.session_id,
                checkpoint
            )
            .is_err()
        );
    }

    #[test]
    fn wrong_session_and_file_only_checkpoints_fail_without_mutating_files() {
        let (root, db, versions, before) = setup();
        let checkpoint = capture(&db, &versions, before.clone(), "paired").unwrap();
        save_changed(&db, &before, checkpoint);
        std::fs::write(root.join("code.txt"), "current").unwrap();
        assert!(restore(&db, &versions, Uuid::new_v4(), checkpoint).is_err());
        let file_only = versions.create_snapshot("legacy").unwrap();
        assert!(restore(&db, &versions, before.session_id, file_only.id).is_err());
        assert_eq!(std::fs::read_to_string(root.join("code.txt")).unwrap(), "current");
        assert!(
            serde_json::to_string(&load_thread(&db, &before.session_id).unwrap())
                .unwrap()
                .contains("discard")
        );
    }

    #[test]
    fn rewinding_to_an_earlier_turn_invalidates_later_branch_checkpoints() {
        let (_, db, versions, before) = setup();
        let first = capture(&db, &versions, before.clone(), "first").unwrap();
        save_changed(&db, &before, first);
        let second_before = load_thread(&db, &before.session_id).unwrap().unwrap();
        let second = capture(&db, &versions, second_before.clone(), "second").unwrap();
        save_changed(&db, &second_before, second);
        restore(&db, &versions, before.session_id, first).unwrap();
        assert!(check_owner(&db, before.session_id, second).is_err());
        assert!(validate(&db, before.session_id, first).is_err());
    }

    #[test]
    fn corrupt_file_blob_keeps_current_session_and_recovers_files() {
        let (root, db, versions, before) = setup();
        let checkpoint = capture(&db, &versions, before.clone(), "before").unwrap();
        save_changed(&db, &before, checkpoint);
        std::fs::write(root.join("code.txt"), "current longer content").unwrap();
        let snapshot =
            versions.list_snapshots().unwrap().into_iter().find(|s| s.id == checkpoint).unwrap();
        db.delete(cf::FILE_BLOBS, snapshot.files["code.txt"].as_bytes()).unwrap();
        assert!(restore(&db, &versions, before.session_id, checkpoint).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("code.txt")).unwrap(),
            "current longer content"
        );
        assert!(
            serde_json::to_string(&load_thread(&db, &before.session_id).unwrap())
                .unwrap()
                .contains("discard")
        );
    }

    #[test]
    fn read_only_restore_failure_does_not_rewind_model_context() {
        let (root, db, versions, before) = setup();
        let checkpoint = capture(&db, &versions, before.clone(), "before").unwrap();
        save_changed(&db, &before, checkpoint);
        std::fs::write(root.join("code.txt"), "current content").unwrap();
        let mut permissions = std::fs::metadata(root.join("code.txt")).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(root.join("code.txt"), permissions).unwrap();
        let error = restore(&db, &versions, before.session_id, checkpoint).unwrap_err().to_string();
        assert!(error.contains("code.txt") && error.contains("no files changed"), "{error}");
        assert!(!error.contains("recovery failed"));
        assert!(
            serde_json::to_string(&load_thread(&db, &before.session_id).unwrap())
                .unwrap()
                .contains("discard")
        );
        assert_eq!(std::fs::read_to_string(root.join("code.txt")).unwrap(), "current content");
    }
}
