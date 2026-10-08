use super::*;

fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name: &str| {
        pairs
            .iter()
            .rev()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    }
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// Eine Push-Pipeline auf einem geschützten Branch — die einzige Lage, in
/// der signiert wird.
const FULL: &[(&str, &str)] = &[
    ("GITLAB_CI", "true"),
    ("CI_PROJECT_PATH", "group/repo"),
    ("CI_PIPELINE_ID", "4711"),
    ("CI_COMMIT_SHA", SHA),
    ("CI_COMMIT_REF_PROTECTED", "true"),
    ("CI_PIPELINE_SOURCE", "push"),
    ("CI_COMMIT_BRANCH", "main"),
    ("CI_DEFAULT_BRANCH", "main"),
    ("MINDS_ANCHOR_KEY_FILE", "/secret/place/anchor_ed25519"),
];

fn with(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut vars = FULL.to_vec();
    vars.extend_from_slice(extra);
    vars
}

#[test]
fn the_pipeline_comes_from_a_protected_push_pipeline() {
    assert_eq!(
        pipeline(&env(FULL)).unwrap(),
        Pipeline {
            project: "group/repo".into(),
            id: 4711,
            sha: SHA.into(),
            before: None,
            branch: "main".into(),
            key: PathBuf::from("/secret/place/anchor_ed25519"),
        }
    );
    let vars = with(&[
        (
            "CI_COMMIT_BEFORE_SHA",
            "0000000000000000000000000000000000000000",
        ),
        ("CI_COMMIT_BRANCH", "main"),
    ]);
    let pipeline = pipeline(&env(&vars)).unwrap();
    // Roh behalten: Dass der Null-Commit „kein Vorgänger" heißt, sagt
    // `range_seals` laut.
    assert_eq!(
        pipeline.before.as_deref(),
        Some("0000000000000000000000000000000000000000")
    );
    assert_eq!(pipeline.branch, "main");
    // Ein `{:?}` zeigt den Schlüsselpfad nie.
    let shown = format!("{pipeline:?}");
    assert!(!shown.contains("/secret/place"), "{shown}");
    assert!(shown.contains("[key]"), "{shown}");
}

/// AC (EA-19): Fehlende CI-Variablen werden alle genannt — und der Lauf
/// sagt, dass er nie mit einem Entwickler-Schlüssel signiert.
#[test]
fn missing_ci_variables_are_named_all_at_once() {
    let err = pipeline(&env(&[("MINDS_ANCHOR_KEY_FILE", "/k")])).unwrap_err();
    assert_eq!(
        err,
        "missing CI variables: GITLAB_CI=true, CI_PROJECT_PATH, CI_PIPELINE_ID, CI_COMMIT_SHA \
         — minds anchor runs only in GitLab CI with the protected anchor key, never with a \
         developer key"
    );
    // Ohne Schlüssel-Variable kein Rückfall — auch nicht in CI.
    let no_key: Vec<_> = FULL
        .iter()
        .copied()
        .filter(|(name, _)| *name != KEY_ENV)
        .collect();
    let err = pipeline(&env(&no_key)).unwrap_err();
    assert!(
        err.starts_with("missing CI variables: MINDS_ANCHOR_KEY_FILE"),
        "{err}"
    );
    // Eine andere CI zählt nicht.
    let err = pipeline(&env(&with(&[("GITLAB_CI", "false")]))).unwrap_err();
    assert!(err.contains("GITLAB_CI=true"), "{err}");
}

/// Security-Review EA-19: Signiert wird nur, wo der Schlüssel keinem
/// unreviewten Code und keinem Aufrufer erreichbar ist — dieselbe Tabelle
/// wie beim signierenden `minds replay`.
#[test]
fn only_protected_push_pipelines_sign() {
    for (extra, expected) in [
        (
            &[("CI_MERGE_REQUEST_IID", "7")][..],
            "refusing to sign in a merge request",
        ),
        (
            &[("CI_PIPELINE_SOURCE", "merge_request_event")][..],
            "refusing to sign in a merge request",
        ),
        (
            &[("CI_PIPELINE_SOURCE", "trigger")][..],
            "refusing to sign in a merge request",
        ),
        (
            &[("CI_PIPELINE_SOURCE", "api")][..],
            "refusing to sign in a merge request",
        ),
        (
            &[("CI_PIPELINE_SOURCE", "web")][..],
            "refusing to sign for the pipeline source web",
        ),
        (
            &[("CI_PIPELINE_SOURCE", "schedule")][..],
            "refusing to sign for the pipeline source schedule",
        ),
        (
            &[("CI_COMMIT_REF_PROTECTED", "false")][..],
            "refusing to sign: this pipeline does not run on a protected ref",
        ),
    ] {
        let err = pipeline(&env(&with(extra))).unwrap_err();
        assert!(err.starts_with(expected), "{extra:?}: {err}");
    }
    let tag: Vec<_> = FULL
        .iter()
        .copied()
        .filter(|(name, _)| *name != "CI_COMMIT_BRANCH")
        .collect();
    assert!(
        pipeline(&env(&tag))
            .unwrap_err()
            .contains("tag pipelines never sign")
    );
    let unprotected: Vec<_> = FULL
        .iter()
        .copied()
        .filter(|(name, _)| *name != "CI_COMMIT_REF_PROTECTED")
        .collect();
    assert!(
        pipeline(&env(&unprotected))
            .unwrap_err()
            .contains("protected ref")
    );
}

/// Ein ungültiger Wert wird benannt, nie zitiert.
#[test]
fn malformed_ci_variables_are_named_not_quoted() {
    for (name, value, expected) in [
        (
            "CI_PROJECT_PATH",
            "group/../x",
            "CI_PROJECT_PATH is not a GitLab project path",
        ),
        (
            "CI_PROJECT_PATH",
            "12345",
            "CI_PROJECT_PATH is not a GitLab project path",
        ),
        (
            "CI_PIPELINE_ID",
            "12; rm",
            "CI_PIPELINE_ID is not a pipeline id",
        ),
    ] {
        let err = pipeline(&env(&with(&[(name, value)]))).unwrap_err();
        assert_eq!(err, expected);
        assert!(!err.contains(value));
    }
}

/// Security-Review EA-19: Die Note braucht einen eigenen Token — nie den,
/// mit dem `verify --online` in Merge-Request-Pipelines prüft.
#[test]
fn the_note_needs_its_own_token() {
    assert_eq!(
        mirror_token(&env(&[])).unwrap_err(),
        "MINDS_ANCHOR_GITLAB_TOKEN is not set — needed for the merge request note"
    );
    let same = [
        ("MINDS_ANCHOR_GITLAB_TOKEN", "glpat-same"),
        ("MINDS_GITLAB_TOKEN", "glpat-same"),
    ];
    let err = mirror_token(&env(&same)).unwrap_err();
    assert!(
        err.starts_with("MINDS_ANCHOR_GITLAB_TOKEN equals MINDS_GITLAB_TOKEN"),
        "{err}"
    );
    assert!(!err.contains("glpat-same"), "{err}");
    let own = [
        ("MINDS_ANCHOR_GITLAB_TOKEN", "glpat-bot"),
        ("MINDS_GITLAB_TOKEN", "glpat-verify"),
    ];
    assert!(mirror_token(&env(&own)).is_ok());
}

/// Nur der Default-Branch wird gegengezeichnet.
#[test]
fn only_the_default_branch_signs() {
    let err = pipeline(&env(&with(&[("CI_COMMIT_BRANCH", "release/1")]))).unwrap_err();
    assert!(err.contains("only the default branch is anchored"), "{err}");
}

/// Die Note geht allein an `CI_SERVER_URL`, nie an `MINDS_GITLAB_URL`
/// (den ein Aufrufer setzen könnte).
#[test]
fn the_note_goes_only_to_the_ci_server() {
    let vars = [
        ("MINDS_GITLAB_TOKEN", "glpat-x"),
        ("MINDS_GITLAB_URL", "https://attacker.example"),
    ];
    let err = mirror_access(&env(&vars), "group/repo").unwrap_err();
    assert_eq!(
        err.to_string(),
        "CI_SERVER_URL is not set — needed for the merge request note"
    );
    let vars = [
        ("MINDS_GITLAB_TOKEN", "glpat-x"),
        ("CI_SERVER_URL", "http://gitlab.example.com"),
    ];
    assert!(mirror_access(&env(&vars), "group/repo").is_err());
}

/// AC (EA-19): Der Schlüsselpfad erscheint in keiner Meldung — auch nicht
/// in der von `ssh-keygen`, die ihn zitiert. Eine kurze relative
/// Schreibweise ersetzt keine gewöhnlichen Wörter.
#[test]
fn key_paths_are_scrubbed_from_messages() {
    let spellings = spellings(
        Path::new("/var/anchor_ed25519"),
        Path::new("/private/var/anchor_ed25519"),
    );
    let message = "ssh-keygen sign failed: Load key \"/var/anchor_ed25519\": invalid format; \
                   /private/var/anchor_ed25519.pub";
    assert_eq!(
        scrub(message, &spellings),
        "ssh-keygen sign failed: Load key \"[key]\": invalid format; [key].pub"
    );
    let short = spellings_of_short();
    assert_eq!(
        scrub("Load key \"/tmp/k\": a key was expected", &short),
        "Load key \"[key]\": a key was expected"
    );
}

fn spellings_of_short() -> Vec<String> {
    spellings(Path::new("k"), Path::new("/tmp/k"))
}

/// Eine öffentliche Schlüsselzeile ist keine Schlüsseldatei: `ssh-keygen`
/// signierte damit über den ssh-agent.
#[test]
fn only_private_key_files_are_keys() {
    let dir = tempfile::tempdir().unwrap();
    let public = dir.path().join("anchor.pub");
    std::fs::write(&public, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA ci\n").unwrap();
    assert!(!private_key_file(&public));
    let private = dir.path().join("anchor");
    std::fs::write(
        &private,
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbg==\n-----END OPENSSH PRIVATE KEY-----\n",
    )
    .unwrap();
    assert!(private_key_file(&private));
    assert!(!private_key_file(&dir.path().join("missing")));
    let pem_public = dir.path().join("anchor.pem");
    std::fs::write(
        &pem_public,
        "-----BEGIN PUBLIC KEY-----\nMCow\n-----END PUBLIC KEY-----\n",
    )
    .unwrap();
    assert!(!private_key_file(&pem_public));
}

/// Security-Review EA-19: Ein Projektpfad mit token-förmigem Segment wird
/// nie signiert — die Default-Redaction findet ihn.
#[test]
fn a_token_shaped_project_path_is_never_signed() {
    let redaction = minds_redact::RedactionConfig::default().pipeline().unwrap();
    let token = concat!("glpat", "-AbCdEfGhIjKlMnOpQrSt");
    assert!(!crate::replay_cmd::clean(
        &redaction,
        &format!("group/{token}")
    ));
    assert!(crate::replay_cmd::clean(&redaction, "group/sub/repo"));
}
