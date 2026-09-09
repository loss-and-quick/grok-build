//! Tests for `x.ai/personas/*`.
//!
//! Every case drives the pure half through explicit roots, so nothing here can
//! reach `~/.grok` even if `grok_home()` has already been resolved by another
//! test in the same binary.

use super::*;

/// Three persona roots under one temp dir, plus the bundle manifest that gates
/// the bundled one.
struct Fixture {
    _tmp: tempfile::TempDir,
    roots: PersonaRoots,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let bundled_root = tmp.path().join("bundled");
        let roots = PersonaRoots {
            bundled: bundled_root.join(PERSONAS_DIR),
            user: tmp.path().join("home").join(PERSONAS_DIR),
            project: Some(tmp.path().join("work").join(".grok").join(PERSONAS_DIR)),
        };
        std::fs::create_dir_all(&roots.bundled).expect("bundled dir");
        std::fs::create_dir_all(&roots.user).expect("user dir");
        std::fs::create_dir_all(roots.project.as_ref().expect("project dir")).expect("project dir");
        Self { _tmp: tmp, roots }
    }

    fn write(&self, scope: PersonaScope, name: &str, content: &str) -> PathBuf {
        let dir = match scope {
            PersonaScope::Bundled => self.roots.bundled.clone(),
            PersonaScope::User => self.roots.user.clone(),
            PersonaScope::Project => self.roots.project.clone().expect("project"),
        };
        let path = dir.join(format!("{name}.toml"));
        std::fs::write(&path, content).expect("write persona");
        if scope == PersonaScope::Bundled {
            self.rewrite_manifest();
        }
        path
    }

    /// List every bundled file in the manifest, so `bundled_stems` sees them.
    fn rewrite_manifest(&self) {
        let mut checksums = std::collections::HashMap::new();
        for stem in stems_in(&self.roots.bundled) {
            checksums.insert(format!("{PERSONAS_DIR}/{stem}.toml"), "x".to_owned());
        }
        let manifest = crate::bundle::BundleManifest {
            version: "test".to_owned(),
            checksums,
        };
        let root = self.roots.bundled.parent().expect("bundled root");
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string(&manifest).expect("manifest json"),
        )
        .expect("write manifest");
    }

    fn user_dir(&self) -> &Path {
        &self.roots.user
    }
}

fn names(list: &[PersonaSummary]) -> Vec<(&str, PersonaScope)> {
    list.iter().map(|p| (p.name.as_str(), p.scope)).collect()
}

#[test]
fn list_merges_three_roots_with_bundled_first() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::Bundled,
        "researcher",
        "description = \"bundled\"\n",
    );
    fx.write(
        PersonaScope::Project,
        "auditor",
        "description = \"project\"\n",
    );
    fx.write(PersonaScope::User, "scribe", "description = \"user\"\n");

    let list = list_at(&fx.roots);
    assert_eq!(
        names(&list),
        vec![
            ("researcher", PersonaScope::Bundled),
            ("auditor", PersonaScope::Project),
            ("scribe", PersonaScope::User),
        ]
    );
}

/// The precedence the pager's own merge had: a bundled name wins over a local
/// one, and a project name wins over a user one.
#[test]
fn a_name_is_claimed_by_the_highest_precedence_root() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::Bundled,
        "shared",
        "description = \"bundled\"\n",
    );
    fx.write(
        PersonaScope::Project,
        "shared",
        "description = \"project\"\n",
    );
    fx.write(PersonaScope::User, "shared", "description = \"user\"\n");
    fx.write(
        PersonaScope::Project,
        "local",
        "description = \"project\"\n",
    );
    fx.write(PersonaScope::User, "local", "description = \"user\"\n");

    let list = list_at(&fx.roots);
    assert_eq!(
        names(&list),
        vec![
            ("shared", PersonaScope::Bundled),
            ("local", PersonaScope::Project),
        ]
    );
}

/// A bundled file the manifest does not claim is a leftover from an older
/// sync, not a persona the agent would load.
#[test]
fn bundled_files_outside_the_manifest_are_not_listed() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::Bundled,
        "current",
        "description = \"bundled\"\n",
    );
    std::fs::write(
        fx.roots.bundled.join("ghost.toml"),
        "description = \"stale\"\n",
    )
    .expect("write ghost");

    assert_eq!(
        names(&list_at(&fx.roots)),
        vec![("current", PersonaScope::Bundled)]
    );
}

#[test]
fn without_a_session_the_project_root_is_not_searched() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::Project,
        "auditor",
        "description = \"project\"\n",
    );
    fx.write(PersonaScope::User, "scribe", "description = \"user\"\n");

    let roots = PersonaRoots {
        bundled: fx.roots.bundled.clone(),
        user: fx.roots.user.clone(),
        project: None,
    };
    assert_eq!(
        names(&list_at(&roots)),
        vec![("scribe", PersonaScope::User)]
    );
}

#[test]
fn description_falls_back_to_the_first_paragraph_of_instructions() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::User,
        "scribe",
        "instructions = \"Take notes.\\n\\nThen file them.\"\n",
    );
    let list = list_at(&fx.roots);
    assert_eq!(list[0].description.as_deref(), Some("Take notes."));
}

#[test]
fn an_unparseable_persona_is_skipped_rather_than_failing_the_catalog() {
    let fx = Fixture::new();
    fx.write(PersonaScope::User, "broken", "{{{ not toml");
    fx.write(PersonaScope::User, "fine", "description = \"ok\"\n");
    assert_eq!(
        names(&list_at(&fx.roots)),
        vec![("fine", PersonaScope::User)]
    );
}

#[test]
fn summaries_report_declared_inputs_and_outputs() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::User,
        "pipeline",
        "[[inputs]]\nname = \"spec\"\ndescription = \"what to build\"\n",
    );
    let list = list_at(&fx.roots);
    assert!(list[0].has_inputs);
    assert!(!list[0].has_outputs);
}

/// A malformed IO entry costs that entry's fields, not the persona: the browser
/// is where a user goes to fix such a file.
#[test]
fn io_entries_missing_fields_still_parse() {
    let fx = Fixture::new();
    fx.write(PersonaScope::User, "loose", "[[inputs]]\nname = \"spec\"\n");
    let doc = get_at(&fx.roots, "loose", None).expect("persona");
    assert_eq!(doc.inputs.len(), 1);
    assert_eq!(doc.inputs[0].io_type, "file");
    assert_eq!(doc.inputs[0].description, "");
    assert!(!doc.inputs[0].required);
}

#[test]
fn get_returns_every_editable_field() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::User,
        "scribe",
        r#"
name = "Scribe"
description = "takes notes"
model = "grok-4"
reasoning_effort = "high"
default_isolation = "worktree"
instructions = "note things"
instructions_file = "notes.md"
"#,
    );
    let doc = get_at(&fx.roots, "scribe", None).expect("persona");
    assert_eq!(doc.name, "scribe");
    assert_eq!(doc.declared_name, "Scribe");
    assert_eq!(doc.description, "takes notes");
    assert_eq!(doc.model, "grok-4");
    assert_eq!(doc.reasoning_effort, "high");
    assert_eq!(doc.default_isolation, "worktree");
    assert_eq!(doc.instructions, "note things");
    assert_eq!(doc.instructions_file, "notes.md");
    assert_eq!(doc.scope, PersonaScope::User);
    assert!(doc.editable);
}

#[test]
fn get_can_be_pinned_to_one_scope() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::Bundled,
        "shared",
        "description = \"bundled\"\n",
    );
    fx.write(PersonaScope::User, "shared", "description = \"user\"\n");

    assert_eq!(
        get_at(&fx.roots, "shared", None).expect("default").scope,
        PersonaScope::Bundled
    );
    let user = get_at(&fx.roots, "shared", Some(PersonaScope::User)).expect("user");
    assert_eq!(user.description, "user");
}

#[test]
fn bundled_personas_are_not_editable() {
    let fx = Fixture::new();
    fx.write(PersonaScope::Bundled, "researcher", "description = \"b\"\n");
    let doc = get_at(&fx.roots, "researcher", None).expect("persona");
    assert!(!doc.editable);
    assert!(!list_at(&fx.roots)[0].editable);
}

/// A read is addressed by an exact stem, so anything shaped like a path is not
/// a persona and is not looked for.
#[test]
fn a_read_refuses_anything_shaped_like_a_path() {
    let fx = Fixture::new();
    for name in ["../escape", "..", ".", "", "a/b", "a\\b"] {
        assert!(get_at(&fx.roots, name, None).is_none(), "get {name:?}");
    }
}

/// A write sanitizes instead of refusing — that is the behaviour the create
/// form has always had — but the separator is what gets sanitized away, so the
/// result is still one file inside the personas directory and never a path out
/// of it.
#[test]
fn a_write_cannot_be_steered_out_of_its_directory() {
    let fx = Fixture::new();
    let escapes = fx.user_dir().parent().expect("parent").join("escape.toml");
    // Distinct names: two that sanitize to the same file would collide on the
    // second create, which is a different refusal than the one under test.
    for name in ["../escape", "a/b", "x\\y", "..\\..\\escape"] {
        let response = save_at(
            fx.user_dir(),
            PersonaScope::User,
            name,
            None,
            &PersonaFields::default(),
        )
        .expect("sanitized rather than refused");
        assert!(response.applied, "save {name:?}");
        let written = Path::new(&response.path);
        assert_eq!(
            written.parent(),
            Some(fx.user_dir()),
            "{name:?} wrote outside the personas directory"
        );
        assert!(!response.name.contains(['/', '\\']), "save {name:?}");
    }
    assert!(!escapes.exists());
}

/// A name with nothing left after sanitizing is refused rather than turned into
/// a file called `---.toml`.
#[test]
fn a_name_that_sanitizes_to_nothing_is_refused() {
    let fx = Fixture::new();
    for name in ["", "..", "///", "!!!"] {
        assert!(
            save_at(
                fx.user_dir(),
                PersonaScope::User,
                name,
                None,
                &PersonaFields::default()
            )
            .is_err(),
            "save {name:?}"
        );
    }
}

#[test]
fn a_name_is_sanitized_into_a_filename_and_reported_back() {
    let fx = Fixture::new();
    let response = save_at(
        fx.user_dir(),
        PersonaScope::User,
        "My Persona!",
        None,
        &PersonaFields {
            description: Some("hi".to_owned()),
            ..PersonaFields::default()
        },
    )
    .expect("save");
    assert!(response.applied);
    assert_eq!(response.name, "My-Persona-");
    assert!(fx.user_dir().join("My-Persona-.toml").exists());
}

#[test]
fn a_name_with_no_alphanumeric_character_is_refused() {
    let fx = Fixture::new();
    assert!(
        save_at(
            fx.user_dir(),
            PersonaScope::User,
            "!!!",
            None,
            &PersonaFields::default()
        )
        .is_err()
    );
}

#[test]
fn a_create_writes_only_the_fields_it_was_given() {
    let fx = Fixture::new();
    let response = save_at(
        fx.user_dir(),
        PersonaScope::User,
        "scribe",
        None,
        &PersonaFields {
            description: Some("takes notes".to_owned()),
            instructions: Some("note things".to_owned()),
            ..PersonaFields::default()
        },
    )
    .expect("save");
    assert!(response.applied);
    let content = std::fs::read_to_string(fx.user_dir().join("scribe.toml")).expect("read");
    assert!(content.contains("description = \"takes notes\""));
    assert!(content.contains("instructions = \"note things\""));
    assert!(!content.contains("model"));
}

#[test]
fn a_create_over_an_existing_name_is_refused_not_overwritten() {
    let fx = Fixture::new();
    fx.write(PersonaScope::User, "scribe", "description = \"original\"\n");
    let response = save_at(
        fx.user_dir(),
        PersonaScope::User,
        "scribe",
        None,
        &PersonaFields {
            description: Some("replacement".to_owned()),
            ..PersonaFields::default()
        },
    )
    .expect("save");
    assert!(!response.applied);
    assert_eq!(
        response.refusal.expect("refusal").kind,
        PersonaRefusalKind::AlreadyExists
    );
    let content = std::fs::read_to_string(fx.user_dir().join("scribe.toml")).expect("read");
    assert!(content.contains("original"));
}

#[test]
fn an_update_quoting_the_current_revision_applies() {
    let fx = Fixture::new();
    fx.write(PersonaScope::User, "scribe", "description = \"before\"\n");
    let doc = get_at(&fx.roots, "scribe", None).expect("persona");

    let response = save_at(
        fx.user_dir(),
        PersonaScope::User,
        "scribe",
        Some(&doc.revision),
        &PersonaFields {
            description: Some("after".to_owned()),
            ..PersonaFields::default()
        },
    )
    .expect("save");
    assert!(response.applied);
    assert_ne!(response.revision.as_deref(), Some(doc.revision.as_str()));
    let content = std::fs::read_to_string(fx.user_dir().join("scribe.toml")).expect("read");
    assert!(content.contains("after"));
}

/// The lost-edit case this whole design exists for: someone else — a second
/// client or the user's own `$EDITOR` — wrote between the read and the save.
#[test]
fn an_update_quoting_a_stale_revision_is_refused_with_the_current_one() {
    let fx = Fixture::new();
    fx.write(PersonaScope::User, "scribe", "description = \"before\"\n");
    let doc = get_at(&fx.roots, "scribe", None).expect("persona");
    fx.write(
        PersonaScope::User,
        "scribe",
        "description = \"hand edit\"\n",
    );

    let response = save_at(
        fx.user_dir(),
        PersonaScope::User,
        "scribe",
        Some(&doc.revision),
        &PersonaFields {
            description: Some("clobber".to_owned()),
            ..PersonaFields::default()
        },
    )
    .expect("save");
    assert!(!response.applied);
    assert_eq!(
        response.refusal.expect("refusal").kind,
        PersonaRefusalKind::Conflict
    );
    let current = get_at(&fx.roots, "scribe", None).expect("persona");
    assert_eq!(
        response.revision.as_deref(),
        Some(current.revision.as_str())
    );
    assert_eq!(current.description, "hand edit");
}

#[test]
fn an_update_of_a_persona_that_was_deleted_is_refused() {
    let fx = Fixture::new();
    fx.write(PersonaScope::User, "scribe", "description = \"before\"\n");
    let doc = get_at(&fx.roots, "scribe", None).expect("persona");
    std::fs::remove_file(fx.user_dir().join("scribe.toml")).expect("remove");

    let response = save_at(
        fx.user_dir(),
        PersonaScope::User,
        "scribe",
        Some(&doc.revision),
        &PersonaFields::default(),
    )
    .expect("save");
    assert!(!response.applied);
    assert_eq!(
        response.refusal.expect("refusal").kind,
        PersonaRefusalKind::NotFound
    );
}

/// Absent means untouched, so an older client that has never heard of a field
/// cannot wipe it, and a newer one need not resend what it did not change.
#[test]
fn an_omitted_field_is_left_alone_and_an_empty_one_is_removed() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::User,
        "scribe",
        "description = \"keep me\"\nmodel = \"grok-4\"\n",
    );
    let doc = get_at(&fx.roots, "scribe", None).expect("persona");

    save_at(
        fx.user_dir(),
        PersonaScope::User,
        "scribe",
        Some(&doc.revision),
        &PersonaFields {
            model: Some(String::new()),
            ..PersonaFields::default()
        },
    )
    .expect("save");

    let after = get_at(&fx.roots, "scribe", None).expect("persona");
    assert_eq!(after.description, "keep me");
    assert_eq!(after.model, "");
}

/// The tables this method does not expose must survive an edit that never
/// mentioned them, which is why the update edits a document rather than
/// re-serializing a struct.
#[test]
fn an_update_preserves_comments_and_untouched_tables() {
    let fx = Fixture::new();
    fx.write(
        PersonaScope::User,
        "pipeline",
        "# hand written\ndescription = \"before\"\n\n[[inputs]]\nname = \"spec\"\ndescription = \"what to build\"\n",
    );
    let doc = get_at(&fx.roots, "pipeline", None).expect("persona");

    save_at(
        fx.user_dir(),
        PersonaScope::User,
        "pipeline",
        Some(&doc.revision),
        &PersonaFields {
            description: Some("after".to_owned()),
            ..PersonaFields::default()
        },
    )
    .expect("save");

    let content = std::fs::read_to_string(fx.user_dir().join("pipeline.toml")).expect("read");
    assert!(content.contains("# hand written"));
    assert!(content.contains("[[inputs]]"));
    assert!(content.contains("after"));
    let after = get_at(&fx.roots, "pipeline", None).expect("persona");
    assert_eq!(after.inputs.len(), 1);
}

#[test]
fn a_write_to_the_bundled_scope_is_refused() {
    let fx = Fixture::new();
    for response in [
        save_at(
            &fx.roots.bundled,
            PersonaScope::Bundled,
            "researcher",
            None,
            &PersonaFields::default(),
        )
        .expect("save"),
        delete_at(&fx.roots.bundled, PersonaScope::Bundled, "researcher", None).expect("delete"),
    ] {
        assert!(!response.applied);
        assert_eq!(
            response.refusal.expect("refusal").kind,
            PersonaRefusalKind::ReadOnly
        );
    }
}

#[test]
fn delete_removes_the_file_and_reports_no_revision() {
    let fx = Fixture::new();
    fx.write(PersonaScope::User, "scribe", "description = \"bye\"\n");
    let response = delete_at(fx.user_dir(), PersonaScope::User, "scribe", None).expect("delete");
    assert!(response.applied);
    assert!(response.revision.is_none());
    assert!(!fx.user_dir().join("scribe.toml").exists());
}

#[test]
fn delete_quoting_a_stale_revision_leaves_the_file_alone() {
    let fx = Fixture::new();
    fx.write(PersonaScope::User, "scribe", "description = \"before\"\n");
    let doc = get_at(&fx.roots, "scribe", None).expect("persona");
    fx.write(
        PersonaScope::User,
        "scribe",
        "description = \"hand edit\"\n",
    );

    let response = delete_at(
        fx.user_dir(),
        PersonaScope::User,
        "scribe",
        Some(&doc.revision),
    )
    .expect("delete");
    assert!(!response.applied);
    assert_eq!(
        response.refusal.expect("refusal").kind,
        PersonaRefusalKind::Conflict
    );
    assert!(fx.user_dir().join("scribe.toml").exists());
}

#[test]
fn deleting_a_persona_that_is_not_there_is_a_refusal_not_an_error() {
    let fx = Fixture::new();
    let response = delete_at(fx.user_dir(), PersonaScope::User, "ghost", None).expect("delete");
    assert!(!response.applied);
    assert_eq!(
        response.refusal.expect("refusal").kind,
        PersonaRefusalKind::NotFound
    );
}

#[test]
fn a_save_creates_the_personas_directory_when_it_is_missing() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path().join("home").join(PERSONAS_DIR);
    let response = save_at(
        &dir,
        PersonaScope::User,
        "scribe",
        None,
        &PersonaFields {
            description: Some("first".to_owned()),
            ..PersonaFields::default()
        },
    )
    .expect("save");
    assert!(response.applied);
    assert!(dir.join("scribe.toml").exists());
}

/// The write is a rename, so no reader can catch a half-written document and a
/// failed write leaves nothing behind to be listed as a persona.
#[test]
fn a_write_leaves_no_temporary_file_behind() {
    let fx = Fixture::new();
    save_at(
        fx.user_dir(),
        PersonaScope::User,
        "scribe",
        None,
        &PersonaFields {
            description: Some("hi".to_owned()),
            ..PersonaFields::default()
        },
    )
    .expect("save");
    let leftovers: Vec<String> = std::fs::read_dir(fx.user_dir())
        .expect("read dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "scribe.toml")
        .collect();
    assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
}

/// A client one version behind sends fewer keys; one version ahead sends more.
/// Neither may fail to parse.
#[test]
fn requests_tolerate_missing_and_unknown_fields() {
    let minimal: PersonaSaveRequest =
        serde_json::from_str(r#"{"scope":"user","name":"scribe"}"#).expect("minimal save parses");
    assert!(minimal.base_revision.is_none());
    assert!(minimal.fields.description.is_none());

    let future: PersonaSaveRequest = serde_json::from_str(
        r#"{"scope":"user","name":"scribe","fields":{"description":"d","colour":"red"},"colour":"red"}"#,
    )
    .expect("unknown fields ignored");
    assert_eq!(future.fields.description.as_deref(), Some("d"));

    let list: PersonaListRequest = serde_json::from_str("{}").expect("empty list request parses");
    assert!(list.session_id.is_none());

    let get: PersonaGetRequest =
        serde_json::from_str(r#"{"name":"scribe"}"#).expect("minimal get parses");
    assert!(get.scope.is_none());

    let delete: PersonaDeleteRequest =
        serde_json::from_str(r#"{"scope":"project","name":"scribe"}"#).expect("minimal delete");
    assert_eq!(delete.scope, PersonaScope::Project);
}

/// The response is the pager's parsing contract; a field that silently changed
/// name would be a field the client stops reading.
#[test]
fn responses_serialize_with_the_names_the_clients_read() {
    let summary = PersonaSummary {
        name: "scribe".to_owned(),
        description: None,
        has_inputs: true,
        has_outputs: false,
        scope: PersonaScope::Project,
        source_path: "/w/.grok/personas/scribe.toml".to_owned(),
        editable: true,
        revision: "abc".to_owned(),
    };
    let json = serde_json::to_value(&summary).expect("serialize");
    assert_eq!(json["hasInputs"], true);
    assert_eq!(json["scope"], "project");
    assert_eq!(json["sourcePath"], "/w/.grok/personas/scribe.toml");
    assert!(json.get("description").is_none(), "absent, not null");

    let refusal = PersonaWriteResponse {
        applied: false,
        name: "scribe".to_owned(),
        scope: PersonaScope::User,
        path: "/h/personas/scribe.toml".to_owned(),
        revision: Some("def".to_owned()),
        refusal: Some(PersonaRefusal {
            kind: PersonaRefusalKind::Conflict,
            message: "changed".to_owned(),
        }),
    };
    let json = serde_json::to_value(&refusal).expect("serialize");
    assert_eq!(json["applied"], false);
    assert_eq!(json["refusal"]["kind"], "conflict");
}
