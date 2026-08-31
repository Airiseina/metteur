//! TUI frame rendering, decoupled from the event loop.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as UiLine, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::app::{AppModel, Kind};

/// Renders the whole frame.
pub fn draw(frame: &mut ratatui::Frame, app: &AppModel) {
    let sidebar_width = 26u16;
    let chunks =
        Layout::vertical([Constraint::Min(5), Constraint::Length(3), Constraint::Length(1)])
            .split(frame.area());
    let main_split = Layout::horizontal([Constraint::Min(40), Constraint::Length(sidebar_width)])
        .split(chunks[0]);

    let visible: Vec<UiLine> = app
        .visible_tail(main_split[0].height.saturating_sub(2) as usize)
        .map(|line| {
            let color = match line.kind {
                Kind::System => Color::Cyan,
                Kind::Event => Color::Gray,
                Kind::Approval => Color::Yellow,
                Kind::Error => Color::Red,
                Kind::Info => Color::White,
            };
            UiLine::from(Span::styled(line.text.clone(), Style::default().fg(color)))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(visible)
            .block(Block::default().borders(Borders::ALL).title(" Events "))
            .wrap(Wrap {
                trim: false,
            }),
        main_split[0],
    );

    let runs_items: Vec<ListItem> = app
        .runs
        .iter()
        .map(|(label, status)| ListItem::new(format!("{label}: {status}")))
        .collect();
    frame.render_widget(
        List::new(runs_items).block(Block::default().borders(Borders::ALL).title(" Runs ")),
        main_split[1],
    );

    let prompt = Span::styled("> ", Style::default().fg(Color::Green));
    let input_line = UiLine::from(vec![prompt, Span::raw(app.input.clone())]);
    frame.render_widget(Paragraph::new(input_line), chunks[1]);

    let note = if app.has_modal() {
        "APPROVAL PENDING — o=once a=run w=ws g=global d/x/D/X=deny esc=dismiss"
    } else {
        "enter=submit up/down=history pgup/pgdn=scroll ctrl+c=quit"
    };
    let status = UiLine::from(Span::styled(
        format!(
            " ws={} | auto-approve={} | {}",
            app.workspace.as_deref().unwrap_or("-"),
            if app.auto_approve {
                "on"
            } else {
                "off"
            },
            note
        ),
        Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(status), chunks[2]);

    if let Some(front) = app.pending.first() {
        let area = centered_rect(60, 20, frame.area());
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(format!(
                "Approval {}\n\n{}\n\nrespond with the shortcut keys shown in the status bar",
                front.request_id, front.detail
            ))
            .block(Block::default().borders(Borders::ALL).title(" Approval required "))
            .wrap(Wrap {
                trim: false,
            }),
            area,
        );
    }
}

/// Computes a centered rectangle for the approval modal.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}