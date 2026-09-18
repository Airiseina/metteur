//! Integration tests for the background-command manager: lifecycle, output
//! capture, killing and ownership.

use metteur_daemon::execution::jobs::{JobManager, JobNoticeKind, JobState};

fn manager() -> JobManager {
    JobManager::new(std::env::temp_dir())
}

/// A command that exits immediately with the given code.
fn exit_with(code: i32) -> String {
    format!("exit {code}")
}

/// A command that stays alive for roughly `seconds`.
///
/// `timeout` cannot be used here: stdin is closed for jobs, and `timeout`
/// refuses to run in that situation on Windows.
fn busy_for(seconds: u32) -> String {
    if cfg!(windows) {
        // `ping -n N` waits about N-1 seconds; the output is discarded.
        format!("ping -n {} 127.0.0.1 > nul", seconds + 1)
    } else {
        format!("sleep {seconds}")
    }
}

#[tokio::test]
async fn a_command_reports_its_exit_code_and_output() {
    let jobs = manager();
    let id = jobs.start("echo hello-job", &std::env::temp_dir(), uuid::Uuid::nil(), 4096).unwrap();
    let snapshot = jobs.wait(&id).await.expect("job is known");
    assert_eq!(
        snapshot.state,
        JobState::Exited {
            code: 0
        }
    );
    assert!(jobs.tail(&id, 10).contains("hello-job"), "{}", jobs.tail(&id, 10));
    assert!(snapshot.finished_at.is_some());
    assert!(snapshot.duration_ms() < 60_000);
}

#[tokio::test]
async fn a_non_zero_exit_is_reported() {
    let jobs = manager();
    let id = jobs.start(&exit_with(3), &std::env::temp_dir(), uuid::Uuid::nil(), 4096).unwrap();
    let snapshot = jobs.wait(&id).await.unwrap();
    assert_eq!(
        snapshot.state,
        JobState::Exited {
            code: 3
        }
    );
    assert_eq!(snapshot.state_label(), "exit 3");
}

#[tokio::test]
async fn stderr_is_merged_into_the_output() {
    let jobs = manager();
    let command = if cfg!(windows) {
        "echo out & echo err 1>&2"
    } else {
        "echo out; echo err 1>&2"
    };
    let id = jobs.start(command, &std::env::temp_dir(), uuid::Uuid::nil(), 4096).unwrap();
    jobs.wait(&id).await.unwrap();
    let tail = jobs.tail(&id, 20);
    assert!(tail.contains("out"), "{tail}");
    assert!(tail.contains("err"), "{tail}");
}

#[tokio::test]
async fn a_large_output_keeps_the_tail() {
    let jobs = manager();
    let id = jobs
        .start("echo 0123456789abcdef", &std::env::temp_dir(), uuid::Uuid::nil(), 8)
        .unwrap();
    let snapshot = jobs.wait(&id).await.unwrap();
    let tail = jobs.tail(&id, 10);
    // Only the last bytes survive the tiny cap; the newest content wins.
    assert!(tail.len() <= 16, "{tail:?}");
    assert!(tail.ends_with("cdef") || tail.contains("cdef"), "{tail:?}");
    assert!(snapshot.output_bytes > tail.len() as u64, "total bytes are tracked");
}

#[tokio::test]
async fn the_output_is_complete_when_the_state_flips() {
    let jobs = manager();
    let command = if cfg!(windows) {
        "echo first & echo second & echo third"
    } else {
        "echo first; echo second; echo third"
    };
    let id = jobs.start(command, &std::env::temp_dir(), uuid::Uuid::nil(), 4096).unwrap();
    let snapshot = jobs.wait(&id).await.unwrap();
    assert!(snapshot.state.is_finished());
    let tail = jobs.tail(&id, 10);
    // The waiter joins the readers before publishing, so nothing is missing.
    for expected in ["first", "second", "third"] {
        assert!(tail.contains(expected), "missing {expected}: {tail:?}");
    }
}

#[tokio::test]
async fn kill_terminates_a_running_process() {
    let jobs = manager();
    let id = jobs.start(&busy_for(30), &std::env::temp_dir(), uuid::Uuid::nil(), 4096).unwrap();
    assert!(jobs.any_running(uuid::Uuid::nil()));
    assert!(jobs.kill(&id));
    let snapshot = jobs.wait(&id).await.unwrap();
    assert_eq!(snapshot.state, JobState::Killed);
    assert_eq!(snapshot.state_label(), "killed");
    assert!(snapshot.duration_ms() < 20_000, "the kill must not wait for the command");
}

#[tokio::test]
async fn killing_a_finished_job_is_a_no_op() {
    let jobs = manager();
    let id = jobs.start("echo done", &std::env::temp_dir(), uuid::Uuid::nil(), 4096).unwrap();
    jobs.wait(&id).await.unwrap();
    assert!(!jobs.kill(&id), "a finished job is left alone");
    assert_eq!(jobs.snapshot(&id).unwrap().state_label(), "exit 0");
}

#[tokio::test]
async fn unknown_ids_are_reported_as_missing() {
    let jobs = manager();
    assert!(jobs.snapshot("nope").is_none());
    assert!(jobs.wait("nope").await.is_none());
    assert_eq!(jobs.tail("nope", 5), "");
    assert!(!jobs.kill("nope"));
}

#[tokio::test]
async fn kill_owned_by_only_touches_its_owner() {
    let jobs = manager();
    let owner = uuid::Uuid::new_v4();
    let other = uuid::Uuid::new_v4();
    let mine = jobs.start(&busy_for(30), &std::env::temp_dir(), owner, 4096).unwrap();
    let theirs = jobs.start(&busy_for(30), &std::env::temp_dir(), other, 4096).unwrap();

    assert_eq!(jobs.kill_owned_by(owner), 1);
    jobs.wait(&mine).await.unwrap();
    assert_eq!(jobs.snapshot(&mine).unwrap().state, JobState::Killed);
    assert!(
        jobs.snapshot(&theirs).unwrap().state.is_running(),
        "another run's job must survive"
    );
    jobs.kill_owned_by(other);
    jobs.wait(&theirs).await.unwrap();
}

#[tokio::test]
async fn wait_any_finished_supports_multiple_jobs() {
    let jobs = manager();
    let owner = uuid::Uuid::new_v4();
    let first = jobs.start("echo one", &std::env::temp_dir(), owner, 4096).unwrap();
    let second = jobs.start(&busy_for(30), &std::env::temp_dir(), owner, 4096).unwrap();

    let finished = jobs.wait_any_finished(owner, &[]).await.expect("one job finished");
    assert_eq!(finished.id, first);

    // The announced job is skipped while the other one still runs.
    assert!(jobs.first_finished(owner, std::slice::from_ref(&first)).is_none());
    jobs.kill_owned_by(owner);
    let mut announced = vec![first];
    let last = jobs.wait_any_finished(owner, &announced).await.expect("the second finished");
    assert_eq!(last.id, second);
    announced.push(last.id.clone());
    // Everything is announced and nothing is running: there is nothing to park on.
    assert!(jobs.wait_any_finished(owner, &announced).await.is_none());
}

#[tokio::test]
async fn listing_reports_running_and_finished_jobs() {
    let jobs = manager();
    let owner = uuid::Uuid::new_v4();
    let done = jobs.start("echo listed", &std::env::temp_dir(), owner, 4096).unwrap();
    jobs.wait(&done).await.unwrap();

    let all = jobs.list(None);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, done);
    assert_eq!(all[0].owner, owner);
    assert!(all[0].summary().contains("exit 0"), "{}", all[0].summary());
    assert!(jobs.list(Some(uuid::Uuid::new_v4())).is_empty());
    assert_eq!(jobs.len(), 1);
    assert!(!jobs.is_empty());
}

#[tokio::test]
async fn a_running_command_streams_output_to_subscribers() {
    // The flush path runs while the job is alive. A lock held across the
    // notice construction here would deadlock the whole manager, so this test
    // also guards the notice plumbing.
    let jobs = manager();
    let mut rx = jobs.subscribe();
    let command = if cfg!(windows) {
        "echo first-line & ping -n 5 127.0.0.1 > nul & echo last-line"
    } else {
        "echo first-line; sleep 3; echo last-line"
    };
    let id = jobs.start(command, &std::env::temp_dir(), uuid::Uuid::nil(), 4096).unwrap();

    let mut streamed_while_running = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout(std::time::Duration::from_secs(8), rx.recv()).await;
        match next {
            Ok(Ok(notice)) => {
                if notice.kind == JobNoticeKind::Finished {
                    break;
                }
                if notice.kind == JobNoticeKind::Output
                    && notice.snapshot.state.is_running()
                    && notice.chunk.contains("first-line")
                {
                    streamed_while_running = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(streamed_while_running, "output must reach subscribers while the command runs");
    // Everything below still works, i.e. no lock was left held.
    assert!(jobs.any_running(uuid::Uuid::nil()));
    assert!(jobs.list(None).len() == 1);
    jobs.kill(&id);
    let snapshot = jobs.wait(&id).await.unwrap();
    assert_eq!(snapshot.state, JobState::Killed);
}
