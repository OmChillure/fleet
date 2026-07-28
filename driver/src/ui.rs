use crate::app::{App, InputMode, Pane};
use crate::model_select::label_for_option;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(f.area());

    draw_header(f, root[0], app);

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(root[1]);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Percentage(28),
            Constraint::Percentage(27),
            Constraint::Percentage(25),
        ])
        .split(body[0]);

    draw_projects(f, top[0], app);
    draw_profiles(f, top[1], app);
    draw_tasks(f, top[2], app);
    draw_inspector(f, top[3], app);
    draw_log(f, body[1], app);
    draw_footer(f, root[2], app);

    // Keep the form visible under nested pickers (profile/project/model).
    if app.form.is_some()
        && matches!(
            app.input_mode,
            InputMode::Form | InputMode::ProfilePicker | InputMode::ModelPicker
        )
    {
        if let Some(form) = &app.form {
            draw_form_modal(f, f.area(), form, app);
        }
    }
    // Draw pickers on top of the form.
    if app.input_mode == InputMode::ProfilePicker {
        if let Some(picker) = &app.profile_picker {
            draw_profile_picker(f, f.area(), picker);
        }
    }
    if app.input_mode == InputMode::ModelPicker {
        if let Some(picker) = &app.model_picker {
            draw_model_picker(f, f.area(), app, picker);
        }
    }
}

fn title_style(active: bool) -> Style {
    if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let run = if app.running { " ● RUNNING" } else { "" };
    let text = format!(
        " Agent Fleet  ·  SQLite {}{run}",
        app.db_path.display()
    );
    let p = Paragraph::new(text)
        .style(Style::default().fg(Color::White).bg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL).title("fleet"));
    f.render_widget(p, area);
}

fn draw_projects(f: &mut Frame, area: Rect, app: &App) {
    let active = app.focus == Pane::Projects;
    let items: Vec<ListItem> = if app.projects.is_empty() {
        vec![ListItem::new(Span::styled(
            "  (empty — n new)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.projects
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let marker = if i == app.selected_project { "› " } else { "  " };
                let line = format!("{marker}{}", p.name);
                let style = sel_style(i == app.selected_project, active);
                ListItem::new(Line::from(Span::styled(line, style)))
            })
            .collect()
    };

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Projects [1] ", title_style(active))),
    );
    f.render_widget(list, area);
}

fn draw_profiles(f: &mut Frame, area: Rect, app: &App) {
    let active = app.focus == Pane::Profiles;
    let items: Vec<ListItem> = if app.profiles.is_empty() {
        vec![ListItem::new(Span::styled(
            "  (empty — n new)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.profiles
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let marker = if i == app.selected_profile { "› " } else { "  " };
                let model = p
                    .model
                    .as_deref()
                    .map(|m| format!(" model:{m}"))
                    .unwrap_or_else(|| " model:?".into());
                let line = format!("{marker}{} [{}]{}", p.name, p.harness, model);
                let style = sel_style(i == app.selected_profile, active);
                ListItem::new(Line::from(Span::styled(line, style)))
            })
            .collect()
    };

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Profiles [2] ", title_style(active))),
    );
    f.render_widget(list, area);
}

fn draw_tasks(f: &mut Frame, area: Rect, app: &App) {
    let active = app.focus == Pane::Tasks;
    let items: Vec<ListItem> = if app.tasks.is_empty() {
        vec![ListItem::new(Span::styled(
            "  (empty — n new)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.tasks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let marker = if i == app.selected_task { "› " } else { "  " };
                let harness = app
                    .profiles
                    .iter()
                    .find(|p| p.id == t.task.profile_id)
                    .map(|p| p.harness.as_str())
                    .unwrap_or("?");
                let line = format!(
                    "{marker}[{}] {} · {}/{}",
                    t.task.status, t.task.title, harness, t.profile_name
                );
                let style = if i == app.selected_task && active {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else if i == app.selected_task {
                    Style::default().fg(Color::Cyan)
                } else {
                    match t.task.status {
                        crate::models::TaskStatus::Running => Style::default().fg(Color::Yellow),
                        crate::models::TaskStatus::Done => Style::default().fg(Color::Green),
                        crate::models::TaskStatus::Failed => Style::default().fg(Color::Red),
                        _ => Style::default(),
                    }
                };
                ListItem::new(Line::from(Span::styled(line, style)))
            })
            .collect()
    };

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Tasks [3] ", title_style(active))),
    );
    f.render_widget(list, area);
}

fn draw_inspector(f: &mut Frame, area: Rect, app: &App) {
    let text = app.inspector_text();
    let p = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" Details · e edit ", Style::default().fg(Color::Gray))),
        );
    f.render_widget(p, area);
}

fn draw_log(f: &mut Frame, area: Rect, app: &App) {
    let active = app.focus == Pane::Log;
    let height = area.height.saturating_sub(2) as usize;
    let total = app.log_lines.len();
    let scroll = app.log_scroll as usize;
    let end = total.saturating_sub(scroll);
    let start = end.saturating_sub(height.max(1));
    let slice = if start < end {
        &app.log_lines[start..end]
    } else {
        &[]
    };

    let text: Vec<Line> = slice.iter().map(|l| Line::from(l.as_str())).collect();
    let p = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(Span::styled(
            " ACP log [4] ",
            title_style(active),
        )));
    f.render_widget(p, area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let content = match app.input_mode {
        InputMode::Normal => app.status.clone(),
        InputMode::Form => {
            if let Some(form) = &app.form {
                if form.editing_value {
                    format!(
                        "Edit {} ({}): {}_",
                        form.current_field().label,
                        form.current_field().hint,
                        form.draft
                    )
                } else {
                    format!(
                        "{} · field {}/{} · s/Ctrl+s save · t template(profile) · Esc cancel",
                        form.title,
                        form.field_idx + 1,
                        form.fields.len()
                    )
                }
            } else {
                app.status.clone()
            }
        }
        InputMode::ConfirmDelete => "Delete selected? [y/N]".into(),
        InputMode::ModelPicker => {
            if let Some(p) = &app.model_picker {
                let what = match p.kind {
                    crate::app::ConfigPickKind::Model => "Model",
                    crate::app::ConfigPickKind::ThoughtLevel => "Thought level",
                };
                if p.custom_mode {
                    format!("Custom {what} for '{}': {}_", p.profile_name, p.custom_input)
                } else {
                    format!(
                        "{what} for '{}' [{}] — j/k · Enter · c custom · f fetch ACP",
                        p.profile_name, p.harness
                    )
                }
            } else {
                app.status.clone()
            }
        }
        InputMode::ProfilePicker => {
            if let Some(p) = &app.profile_picker {
                format!("{} — j/k · Enter", p.title)
            } else {
                app.status.clone()
            }
        }
    };
    let help =
        " n new · e edit · m model · l thought · h harness · d delete · r run · q quit ";
    let p = Paragraph::new(vec![
        Line::from(Span::styled(content, Style::default().fg(Color::White))),
        Line::from(Span::styled(help, Style::default().fg(Color::DarkGray))),
    ])
    .block(Block::default().borders(Borders::ALL).title("status"));
    f.render_widget(p, area);
}

fn draw_form_modal(f: &mut Frame, area: Rect, form: &crate::form::Form, _app: &App) {
    let popup = centered_rect(78, 80, area);
    f.render_widget(Clear, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(4)])
        .split(popup);

    let items: Vec<ListItem> = form
        .fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let marker = if i == form.field_idx { "›" } else { " " };
            let preview = if field.value.len() > 60 {
                format!("{}…", &field.value.chars().take(60).collect::<String>())
            } else if field.value.is_empty() {
                "(empty)".into()
            } else {
                field.value.clone()
            };
            let line = format!("{marker} {:<14} {}", field.label, preview);
            let style = if i == form.field_idx {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(line, style)))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", form.title))
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(list, chunks[0]);

    let field = form.current_field();
    let value_view = if form.editing_value {
        format!("{}_", form.draft)
    } else {
        field.value.clone()
    };
    let enter_hint = if matches!(field.key, "profile" | "project") {
        "★ Enter/Space = LIST PICKER (profile/project)"
    } else if matches!(field.key, "model" | "thought_level") {
        "★ Enter/Space = LIVE MODEL/THOUGHT PICKER (from ACP)"
    } else {
        "Enter = edit field text"
    };
    let help = Paragraph::new(vec![
        Line::from(format!("{} — {}", field.label, field.hint)),
        Line::from(value_view),
        Line::from(format!(
            "{enter_hint} · j/k next · m model · l thought · s save · Esc"
        )),
    ])
    .wrap(Wrap { trim: false })
    .block(Block::default().borders(Borders::ALL).title(" field "));
    f.render_widget(help, chunks[1]);
}

fn sel_style(selected: bool, active: bool) -> Style {
    if selected && active {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn draw_profile_picker(f: &mut Frame, area: Rect, picker: &crate::app::ProfilePicker) {
    let popup = centered_rect(72, 70, area);
    f.render_widget(Clear, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(popup);

    let items: Vec<ListItem> = picker
        .options
        .iter()
        .enumerate()
        .map(|(i, (_id, label))| {
            let marker = if i == picker.selected { "›" } else { " " };
            let style = if i == picker.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!("{marker} {label}"),
                style,
            )))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", picker.title))
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(list, chunks[0]);

    let help = Paragraph::new(match picker.kind {
        crate::app::RefPickKind::Profile => {
            "Select which ACP profile runs this task (harness + model + MCP).\n\
             j/k move · Enter confirm · Esc cancel"
        }
        crate::app::RefPickKind::Project => {
            "Select which project folder (cwd) this task runs in.\n\
             j/k move · Enter confirm · Esc cancel"
        }
    })
    .wrap(Wrap { trim: false })
    .block(Block::default().borders(Borders::ALL).title(" help "));
    f.render_widget(help, chunks[1]);
}

fn draw_model_picker(
    f: &mut Frame,
    area: Rect,
    app: &App,
    picker: &crate::app::ModelPicker,
) {
    let popup = centered_rect(80, 85, area);
    f.render_widget(Clear, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(5)])
        .split(popup);

    let profile = app
        .profiles
        .iter()
        .find(|p| p.id == picker.profile_id);

    let total = picker.options.len();
    let items: Vec<ListItem> = picker
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let label = if opt == "__custom__" {
                "✎ type custom id…".into()
            } else if opt.is_empty() {
                "(agent default)".into()
            } else {
                let pretty = pretty_config_label(profile, picker.kind, opt);
                if pretty != *opt {
                    format!("{pretty}  ({opt})")
                } else {
                    label_for_option(opt)
                }
            };
            // Index badge helps orientation in long lists
            let line = if total > 20 {
                format!("{:>3}. {}", i, label)
            } else {
                label
            };
            ListItem::new(Line::from(line))
        })
        .collect();

    let n_models = profile.map(|p| p.discovered.models.len()).unwrap_or(0);
    let n_thought = profile
        .map(|p| p.discovered.thought_levels.len())
        .unwrap_or(0);
    let fetch_tag = if picker.fetching { " · fetching…" } else { "" };
    let pos = if total == 0 {
        0
    } else {
        picker.selected + 1
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    " {} · {} [{}] · {}/{} · {} models / {} thought{} ",
                    match picker.kind {
                        crate::app::ConfigPickKind::Model => "Model",
                        crate::app::ConfigPickKind::ThoughtLevel => "Thought level",
                    },
                    picker.profile_name,
                    picker.harness,
                    pos,
                    total,
                    n_models,
                    n_thought,
                    fetch_tag
                ))
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");

    // Stateful list keeps the selection scrolled into view (critical for 100s of models).
    let mut state = ListState::default();
    if total > 0 {
        state.select(Some(picker.selected.min(total - 1)));
    }
    f.render_stateful_widget(list, chunks[0], &mut state);

    let body = if picker.custom_mode {
        format!(
            "Type value (empty = agent default):\n{}_\nEnter save · Esc back to list",
            picker.custom_input
        )
    } else if picker.fetching {
        "Fetching model list from the ACP agent…\n\
         Live options replace this list when ready.\n\
         Esc cancel"
            .into()
    } else if n_models == 0 && matches!(picker.kind, crate::app::ConfigPickKind::Model) {
        "Agent returned no models — press f to retry, or c for custom id.\n\
         j/k · PgUp/PgDn · Home/End · Enter · c · f · Esc"
            .into()
    } else if n_thought == 0 && matches!(picker.kind, crate::app::ConfigPickKind::ThoughtLevel) {
        "Agent returned no thought/effort levels — press f to retry, or c for custom.\n\
         j/k · PgUp/PgDn · Home/End · Enter · c · f · Esc"
            .into()
    } else {
        format!(
            "Live ACP list ({total} options). j/k · PgUp/PgDn scroll · Home/End · Enter apply · c custom · f re-fetch · Esc"
        )
    };
    let help = Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(" help "));
    f.render_widget(help, chunks[1]);
}

fn pretty_config_label(
    profile: Option<&crate::models::HarnessProfile>,
    kind: crate::app::ConfigPickKind,
    value: &str,
) -> String {
    let Some(p) = profile else {
        return value.to_string();
    };
    let choices = match kind {
        crate::app::ConfigPickKind::Model => &p.discovered.models,
        crate::app::ConfigPickKind::ThoughtLevel => &p.discovered.thought_levels,
    };
    choices
        .iter()
        .find(|c| c.value == value)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| value.to_string())
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
