use super::*;
use crossterm::event::KeyModifiers;

/// A persona as `x.ai/personas/get` hands it over. Nothing here touches a file:
/// the view no longer reads one, and a committed edit leaves as a request.
fn editable_state() -> PersonaDetailState {
    PersonaDetailState::from_document(&PersonaDocument {
        name: "reviewer".to_owned(),
        declared_name: "reviewer".to_owned(),
        description: "old description".to_owned(),
        model: "grok".to_owned(),
        reasoning_effort: "high".to_owned(),
        default_isolation: "worktree".to_owned(),
        instructions: "read only instructions".to_owned(),
        instructions_file: String::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        scope: PersonaScope::Project,
        source_path: "/w/.grok/personas/reviewer.toml".to_owned(),
        editable: true,
        revision: "rev-1".to_owned(),
    })
}

fn press(state: &mut PersonaDetailState, code: KeyCode) -> PersonaDetailOutcome {
    handle_persona_detail_key(state, &KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn detail_edit_commit_asks_the_shell_to_save_quoting_the_revision() {
    let mut state = editable_state();
    state.selected_field = PersonaField::Description;
    let _ = press(&mut state, KeyCode::Enter);
    let _ = press(&mut state, KeyCode::Home);
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
    );
    for ch in "new description".chars() {
        let _ = press(&mut state, KeyCode::Char(ch));
    }
    let outcome = press(&mut state, KeyCode::Enter);

    assert!(!state.is_editing());
    assert_eq!(state.description, "new description");
    assert!(state.dirty);
    match outcome {
        PersonaDetailOutcome::Save {
            name,
            scope,
            base_revision,
            fields,
        } => {
            // Addressed by catalog name and scope, never by the path the view
            // happens to display.
            assert_eq!(name, "reviewer");
            assert_eq!(scope, PersonaScope::Project);
            assert_eq!(base_revision, "rev-1");
            assert_eq!(fields.description, "new description");
            // Every field goes, so a cleared one is cleared rather than kept.
            assert_eq!(fields.model, "grok");
        }
        other => panic!("expected a save request, got {other:?}"),
    }
}

/// The refusal path: the file moved on while the editor was open. The view says
/// so and keeps the stale revision, so the next keystroke cannot turn the
/// refusal into an overwrite.
#[test]
fn a_conflicting_save_keeps_the_stale_revision_and_says_to_reopen() {
    let mut state = editable_state();
    state.apply_write_result(&PersonaWriteResponse {
        applied: false,
        name: "reviewer".to_owned(),
        scope: PersonaScope::Project,
        path: "/w/.grok/personas/reviewer.toml".to_owned(),
        revision: Some("rev-2".to_owned()),
        refusal: Some(xai_grok_shell::extensions::personas::PersonaRefusal {
            kind: PersonaRefusalKind::Conflict,
            message: "persona 'reviewer' changed on disk since it was read".to_owned(),
        }),
    });
    assert_eq!(state.revision, "rev-1");
    assert!(state.message.as_deref().unwrap().contains("reopen"));
}

#[test]
fn an_applied_save_adopts_the_new_revision_and_clears_dirty() {
    let mut state = editable_state();
    state.dirty = true;
    state.apply_write_result(&PersonaWriteResponse {
        applied: true,
        name: "reviewer".to_owned(),
        scope: PersonaScope::Project,
        path: "/w/.grok/personas/reviewer.toml".to_owned(),
        revision: Some("rev-2".to_owned()),
        refusal: None,
    });
    assert_eq!(state.revision, "rev-2");
    assert!(!state.dirty);
    assert_eq!(state.message.as_deref(), Some("Saved"));
}

#[test]
fn detail_edit_cancel_preserves_original_and_asks_for_nothing() {
    let mut state = editable_state();
    state.selected_field = PersonaField::Name;
    let _ = press(&mut state, KeyCode::Enter);
    let _ = press(&mut state, KeyCode::Char('X'));
    let outcome = press(&mut state, KeyCode::Esc);

    assert!(!state.is_editing());
    assert_eq!(state.name, "reviewer");
    assert!(!state.dirty);
    assert!(matches!(outcome, PersonaDetailOutcome::Changed));
}

#[test]
fn detail_unchanged_edit_asks_for_nothing_and_does_not_mark_dirty() {
    let mut state = editable_state();
    state.selected_field = PersonaField::Model;
    let _ = press(&mut state, KeyCode::Enter);
    let outcome = press(&mut state, KeyCode::Enter);

    assert!(!state.is_editing());
    assert!(!state.dirty);
    assert!(state.message.is_none());
    assert!(matches!(outcome, PersonaDetailOutcome::Changed));
}

/// A bundled persona is read-only on the wire too, so the view refuses before
/// it can compose a request the shell would only refuse again.
#[test]
fn a_bundled_persona_refuses_the_edit_key() {
    let mut state = PersonaDetailState::from_document(&PersonaDocument {
        name: "researcher".to_owned(),
        scope: PersonaScope::Bundled,
        editable: false,
        ..Default::default()
    });
    state.selected_field = PersonaField::Description;
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(matches!(outcome, PersonaDetailOutcome::Changed));
    assert!(!state.is_editing());
    assert_eq!(
        state.message.as_deref(),
        Some("Bundled personas are read-only")
    );
}

/// A persona that declares no `name` key shows its catalog name, and a save
/// still addresses the catalog name rather than the displayed one.
#[test]
fn an_undeclared_name_falls_back_to_the_catalog_name() {
    let state = PersonaDetailState::from_document(&PersonaDocument {
        name: "scribe".to_owned(),
        declared_name: String::new(),
        scope: PersonaScope::User,
        editable: true,
        ..Default::default()
    });
    assert_eq!(state.name, "scribe");
    assert_eq!(state.catalog_name, "scribe");
    assert_eq!(state.scope, PersonaScope::User);
}

#[test]
fn multiline_values_require_source_file_editing() {
    let mut state = editable_state();
    state.description = "first line\nsecond line".to_owned();
    state.selected_field = PersonaField::Description;

    let outcome = press(&mut state, KeyCode::Enter);
    assert!(matches!(outcome, PersonaDetailOutcome::Changed));
    assert!(!state.is_editing());
    assert_eq!(state.description, "first line\nsecond line");
    assert_eq!(
        state.message.as_deref(),
        Some("Multiline values must be edited in the source file")
    );
}

#[test]
fn detail_instructions_remain_read_only_inline() {
    let mut state = editable_state();
    state.selected_field = PersonaField::Instructions;
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(matches!(outcome, PersonaDetailOutcome::Changed));
    assert!(!state.is_editing());
    assert!(state.instructions_expanded);
}

#[test]
fn detail_paste_targets_only_active_editor_and_sanitizes() {
    let mut state = editable_state();
    state.selected_field = PersonaField::Model;
    let _ = press(&mut state, KeyCode::Enter);
    state.set_editing_text("ab");
    let _ = state.set_editing_cursor_byte(1);
    let outcome = handle_persona_detail_paste(&mut state, "中\r\n");
    assert!(matches!(outcome, PersonaDetailOutcome::Changed));
    assert_eq!(state.editing_text(), Some("a中b"));

    let _ = press(&mut state, KeyCode::Esc);
    let outcome = handle_persona_detail_paste(&mut state, "ignored");
    assert!(matches!(outcome, PersonaDetailOutcome::Unchanged));
}

#[test]
fn detail_editor_uses_canonical_graphemes_and_keeps_cursor_visible() {
    let mut state = editable_state();
    state.selected_field = PersonaField::Model;
    let _ = press(&mut state, KeyCode::Enter);
    let grapheme = "👩🏽\u{200d}💻";
    state.set_editing_text(format!("a{grapheme}b"));
    let _ = state.set_editing_cursor_byte(1);
    let _ = press(&mut state, KeyCode::Delete);
    assert_eq!(state.editing_text(), Some("ab"));

    let text = format!("123456中e\u{301}{grapheme}z");
    state.set_editing_text(&text);
    let _ = state.set_editing_cursor_byte(text.len() - 1);

    let width = 10usize;
    let theme = Theme::current();
    let mut buffer = Buffer::empty(Rect::new(0, 0, width as u16, 1));
    let viewport = state.editing_viewport(width).unwrap();
    let visible = &state.editing_text().unwrap()[viewport.visible_byte_range.clone()];
    assert!(visible.contains('中'));
    assert!(visible.contains("e\u{301}"));
    assert!(visible.contains(grapheme));
    render_detail_editor(
        &mut buffer,
        0,
        0,
        width,
        state.editing_editor().unwrap(),
        Style::default(),
        &theme,
    );
    let cursor_x = viewport.cursor_display_column as u16;
    assert_eq!(buffer[(cursor_x, 0)].bg, theme.text_primary);
}
