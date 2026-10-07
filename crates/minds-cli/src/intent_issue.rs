//! Der GitLab-Zugang für Issue-Anker (EA-16): `minds intent bind --issue`
//! und `minds verify --online`.
//!
//! # Woher Instanz und Token kommen
//!
//! - Instanz: `--gitlab-url`, sonst `MINDS_GITLAB_URL`, sonst
//!   `CI_SERVER_URL` (in GitLab-CI gesetzt). **Nie** aus `.git/config` (anders
//!   als `minds gitlab mirror`): Die Datei kann der Agent schreiben. Ein
//!   umgebogenes `minds.gitlabUrl` schickte den Token an seinen Server und
//!   ließe ihn dort ein „passendes" Issue ausliefern — `verify` meldete eine
//!   Fassung als `current`, die es bei GitLab nie gab.
//! - Nur `https://`; `http://` allein für die Loopback-Literale `127.0.0.1`
//!   und `[::1]` (Tests) — der Token ginge sonst im Klartext über das Netz.
//!   In GitLab-CI geht `MINDS_GITLAB_URL` vor `CI_SERVER_URL`; beide
//!   stehen unter der Kontrolle dessen, der die CI-Konfiguration ändert —
//!   wer das kann, erreicht den Token ohnehin.
//! - Token: nur die Umgebungsvariable `MINDS_GITLAB_TOKEN`, über stdin an
//!   curl ([`minds_gitlab::Project`]); er steht in keinem Fehlertext.
//!
//! # Was die Prüfung bedeutet
//!
//! [`OnlineCheck::version`] gibt ein [`IssueVersion`] — einen Lesezeit-Befund,
//! den `verify` als eigene Zeile zeigt. Er ändert weder Assurance noch
//! Verdikt noch Exit-Code (W2, W6): Ohne `--online` heißt er
//! `not checked (offline)`, eine gescheiterte Prüfung
//! `version check unavailable (…)` — nie „gültig".

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use minds_core::ContentHash;
use minds_core::intent_anchor::{IntentAnchor, IntentSource, IssueVersion, content_hash};
use minds_gitlab::Project;

/// Die Umgebungsvariable mit dem Token.
pub(crate) const TOKEN_ENV: &str = "MINDS_GITLAB_TOKEN";

/// Die Umgebungsvariable mit der Instanz.
const URL_ENV: &str = "MINDS_GITLAB_URL";

/// Die Instanz, die GitLab-CI jedem Job mitgibt.
const CI_URL_ENV: &str = "CI_SERVER_URL";

/// Die Instanz: Flag, dann Umgebung. Geprüft wie in der Modul-Doku.
pub(crate) fn base_url(flag: Option<&str>) -> Result<String, &'static str> {
    let url = flag
        .map(str::to_owned)
        .or_else(|| env(URL_ENV))
        .or_else(|| env(CI_URL_ENV))
        .ok_or("no GitLab instance: --gitlab-url <url>, MINDS_GITLAB_URL or CI_SERVER_URL")?;
    check_url(&url)?;
    Ok(url)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// `https://host[:port][/pfad]`, oder `http://` zu Loopback. Keine
/// Zugangsdaten in der URL, kein Leer- oder Steuerzeichen, keine Query.
fn check_url(url: &str) -> Result<(), &'static str> {
    const BAD: &str = "the GitLab URL must look like https://gitlab.example.com";
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) || url.contains(['?', '#', '@']) {
        return Err(BAD);
    }
    let (https, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(BAD);
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        // Ein IPv6-Literal braucht seine schließende Klammer.
        Some(v6) => v6.split_once(']').map(|(host, _)| host).ok_or(BAD)?,
        None => authority.split(':').next().unwrap_or_default(),
    };
    if host.is_empty() {
        return Err(BAD);
    }
    // Nur IP-Literale: `localhost` hinge an der Namensauflösung.
    if !https && !matches!(host, "127.0.0.1" | "::1") {
        return Err("the GitLab URL must use https:// (http:// only for 127.0.0.1 or [::1])");
    }
    Ok(())
}

/// Der Zugang zum Projekt `path` (`gruppe/projekt`) auf `base`.
pub(crate) fn project(base: &str, path: &str) -> Result<Project, minds_gitlab::TokenError> {
    Project::new(base, &path.replace('/', "%2F"), TOKEN_ENV)
}

/// Die Versionsprüfung eines Laufs von `minds verify` — je Anker einmal
/// gerechnet, auch wenn mehrere Sessions ihn tragen.
pub(crate) struct OnlineCheck {
    /// `None`: ohne `--online`. Sonst die Instanz — oder warum es keine gibt.
    access: Option<Result<String, &'static str>>,
    /// Die Wurzel des Worktrees, für die Redaction-Policy.
    root: PathBuf,
    cache: RefCell<HashMap<ContentHash, IssueVersion>>,
}

impl OnlineCheck {
    /// `online == false`: Jeder Issue-Anker ist `not checked (offline)`.
    pub(crate) fn new(online: bool, gitlab_url: Option<&str>, root: PathBuf) -> Self {
        Self {
            access: online.then(|| base_url(gitlab_url)),
            root,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Der Befund für einen Anker — `None`, wenn er nicht auf ein Issue zeigt.
    pub(crate) fn version(&self, id: &ContentHash, anchor: &IntentAnchor) -> Option<IssueVersion> {
        let IntentSource::Issue {
            project,
            iid,
            updated_at,
        } = &anchor.source
        else {
            return None;
        };
        if let Some(known) = self.cache.borrow().get(id) {
            return Some(*known);
        }
        let version = match &self.access {
            None => IssueVersion::NotChecked,
            Some(Err(reason)) => IssueVersion::Unavailable(reason),
            Some(Ok(base)) => self.check(base, project, *iid, updated_at, &anchor.content),
        };
        self.cache.borrow_mut().insert(id.clone(), version);
        Some(version)
    }

    fn check(
        &self,
        base: &str,
        path: &str,
        iid: u64,
        updated_at: &str,
        content: &ContentHash,
    ) -> IssueVersion {
        let project = match project(base, path) {
            Ok(project) => project,
            Err(minds_gitlab::TokenError::Missing(_)) => {
                return IssueVersion::Unavailable("MINDS_GITLAB_TOKEN is not set");
            }
            Err(minds_gitlab::TokenError::Malformed(_)) => {
                return IssueVersion::Unavailable("MINDS_GITLAB_TOKEN is not a valid token");
            }
        };
        // Dieselbe Policy und dieselbe Funktion wie beim Binden.
        let Ok(pipeline) =
            crate::config::load_redaction(&self.root).and_then(|config| Ok(config.pipeline()?))
        else {
            return IssueVersion::Unavailable("redaction policy unreadable");
        };
        minds_gitlab::check_version(
            &project,
            iid,
            updated_at,
            |title, description| match pipeline
                .redacted_issue_snapshot(title.to_owned(), description.to_owned())
            {
                Ok(snapshot) => Ok(content_hash(snapshot.as_bytes()) == *content),
                Err(minds_redact::RedactionError::IntentSnapshotTooLarge) => {
                    Err("issue larger than an intent snapshot may be")
                }
                Err(_) => Err("issue text refused by the redaction policy"),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_loopback_http_is_accepted() {
        for good in [
            "https://gitlab.com",
            "https://gitlab.example.com:8443/gitlab",
            "http://127.0.0.1:41234",
            "http://[::1]:9000",
        ] {
            assert_eq!(check_url(good), Ok(()), "{good}");
        }
        for bad in [
            "http://localhost:8080",
            "http://[::1",
            "https://[::1:443",
            "http://gitlab.example.com",
            "ftp://gitlab.example.com",
            "gitlab.example.com",
            "https://user:pw@gitlab.example.com",
            "https://gitlab.example.com/?x=1",
            "https://gitlab.example.com\n",
            "https://",
            "http://127.0.0.1.evil.example",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    fn issue_anchor() -> IntentAnchor {
        IntentAnchor {
            source: IntentSource::Issue {
                project: "team/minds".into(),
                iid: 42,
                updated_at: "2026-10-01T08:15:00Z".into(),
            },
            content: content_hash(b"{}"),
            scope: Vec::new(),
        }
    }

    /// Ohne `--online` wird nichts gefragt — und nichts behauptet.
    #[test]
    fn offline_is_not_checked_and_files_have_no_version() {
        let check = OnlineCheck::new(false, None, PathBuf::from("/nonexistent"));
        let id = IntentAnchor::id_of_text(&issue_anchor().to_text().unwrap());
        assert_eq!(
            check.version(&id, &issue_anchor()),
            Some(IssueVersion::NotChecked)
        );
        let file = IntentAnchor {
            source: IntentSource::Prompt,
            ..issue_anchor()
        };
        assert_eq!(check.version(&id, &file), None);
    }

    /// `--online` mit einer unzulässigen Instanz: nicht verfügbar, mit
    /// festem Grund — es geht keine Anfrage hinaus.
    #[test]
    fn a_bad_instance_is_unavailable_without_a_request() {
        let check = OnlineCheck::new(
            true,
            Some("http://gitlab.example.com"),
            PathBuf::from("/nonexistent"),
        );
        let id = IntentAnchor::id_of_text(&issue_anchor().to_text().unwrap());
        assert_eq!(
            check.version(&id, &issue_anchor()),
            Some(IssueVersion::Unavailable(
                "the GitLab URL must use https:// (http:// only for 127.0.0.1 or [::1])"
            ))
        );
    }
}
