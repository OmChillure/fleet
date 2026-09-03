//! One git remote per project (multi-repo is future).

/// Normalized project git binding. `url` is a clone URL; `git_ref` is branch, tag, or commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGitRemote {
    pub url: String,
    pub git_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitRemoteError {
    #[error("{0}")]
    Invalid(String),
}

/// Parse optional form fields into a binding.
///
/// Empty `url` means "no repo" (clears the project binding). Empty `git_ref`
/// defaults to `main`.
pub fn parse_project_git_remote(
    url: Option<&str>,
    git_ref: Option<&str>,
) -> Result<Option<ProjectGitRemote>, GitRemoteError> {
    let url = url.map(str::trim).filter(|s| !s.is_empty());
    let git_ref = normalize_git_ref(git_ref)?;
    match url {
        None => Ok(None),
        Some(url) => {
            validate_git_url(url)?;
            Ok(Some(ProjectGitRemote {
                url: url.to_string(),
                git_ref,
            }))
        }
    }
}

pub fn normalize_git_ref(raw: Option<&str>) -> Result<String, GitRemoteError> {
    let s = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("main");
    validate_git_ref(s)?;
    Ok(s.to_string())
}

pub fn validate_git_url(url: &str) -> Result<(), GitRemoteError> {
    if url.chars().any(|c| c.is_whitespace() || c == '\0') {
        return Err(GitRemoteError::Invalid(
            "repo_url must not contain whitespace".into(),
        ));
    }
    if url.starts_with('-') {
        return Err(GitRemoteError::Invalid(
            "repo_url must not start with '-'".into(),
        ));
    }
    let ok = url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("ssh://")
        || url.starts_with("git://")
        || url.starts_with("file://")
        || url.starts_with("git@");
    if !ok {
        return Err(GitRemoteError::Invalid(
            "repo_url must be an http(s), ssh, git, file, or git@ URL".into(),
        ));
    }
    // Cookie GET /fleet and spawn FLEET_REPO_URL echo this string; do not persist user:pass@.
    if http_url_has_userinfo(url) {
        return Err(GitRemoteError::Invalid(
            "repo_url must not contain credentials".into(),
        ));
    }
    Ok(())
}

fn http_url_has_userinfo(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    rest[..authority_end].contains('@')
}

pub fn validate_git_ref(git_ref: &str) -> Result<(), GitRemoteError> {
    if git_ref.is_empty() {
        return Err(GitRemoteError::Invalid("git_ref is required".into()));
    }
    if git_ref.chars().any(|c| c.is_whitespace() || c == '\0') {
        return Err(GitRemoteError::Invalid(
            "git_ref must not contain whitespace".into(),
        ));
    }
    if git_ref.starts_with('-') || git_ref.contains("..") {
        return Err(GitRemoteError::Invalid(
            "git_ref is not a valid git ref".into(),
        ));
    }
    Ok(())
}

/// Apply a PATCH of `repo_url` / `git_ref` against the current project binding.
///
/// Shared by cookie `PATCH …/fleet/config` and admin `PATCH /v1/projects/{id}/config`
/// (Docs §6.9 / §8.2). Empty / whitespace `repo_url` clears the remote. Omitted
/// `repo_url` keeps the current URL and optionally updates `git_ref`. `git_ref`
/// without a current URL is an error. Non-empty `repo_url` is validated.
pub fn apply_git_remote_patch(
    existing: Option<&ProjectGitRemote>,
    existing_git_ref: &str,
    repo_url: Option<&str>,
    git_ref: Option<&str>,
) -> Result<Option<ProjectGitRemote>, GitRemoteError> {
    match repo_url {
        None => match existing {
            Some(r) => {
                let mut out = r.clone();
                if let Some(rf) = git_ref {
                    out.git_ref = normalize_git_ref(Some(rf))?;
                }
                Ok(Some(out))
            }
            None => {
                if git_ref.is_some() {
                    Err(GitRemoteError::Invalid(
                        "git_ref requires repo_url on this project".into(),
                    ))
                } else {
                    Ok(None)
                }
            }
        },
        Some(url) => parse_project_git_remote(Some(url), git_ref.or(Some(existing_git_ref))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(url: &str, git_ref: &str) -> ProjectGitRemote {
        ProjectGitRemote {
            url: url.into(),
            git_ref: git_ref.into(),
        }
    }

    fn err_text(url: Option<&str>, git_ref: Option<&str>) -> String {
        parse_project_git_remote(url, git_ref)
            .expect_err("expected invalid remote")
            .to_string()
    }

    #[test]
    fn empty_url_clears_binding() {
        assert_eq!(
            parse_project_git_remote(Some("  "), Some("main")).unwrap(),
            None
        );
        assert_eq!(
            parse_project_git_remote(None, Some("develop")).unwrap(),
            None
        );
    }

    #[test]
    fn https_ok() {
        let r = parse_project_git_remote(Some("https://github.com/acme/app.git"), Some("develop"))
            .unwrap()
            .unwrap();
        assert_eq!(r.url, "https://github.com/acme/app.git");
        assert_eq!(r.git_ref, "develop");
    }

    #[test]
    fn default_ref_is_main() {
        let r = parse_project_git_remote(Some("https://example.com/r.git"), None)
            .unwrap()
            .unwrap();
        assert_eq!(r.git_ref, "main");
    }

    #[test]
    fn rejects_garbage_url() {
        for bad in [
            "not a url",
            "-e evil",
            "ftp://example.com/r.git",
            "https://example.com/has space.git",
            "javascript:alert(1)",
            "https://user:unique-secret@example.test/acme.git",
            "http://token:x-oauth-basic@github.com/acme/app.git",
        ] {
            let msg = err_text(Some(bad), None);
            assert!(
                msg.contains("repo_url"),
                "garbage {bad:?} must mention repo_url: {msg}"
            );
            assert!(
                !msg.contains("unique-secret") && !msg.contains("x-oauth-basic"),
                "error must not echo credentials: {msg}"
            );
        }
    }

    #[test]
    fn rejects_http_userinfo_without_echoing_secret() {
        let msg = err_text(
            Some("https://user:unique-secret@example.test/acme.git"),
            None,
        );
        assert!(
            msg.contains("credentials"),
            "userinfo reject must mention credentials: {msg}"
        );
        assert!(
            !msg.contains("unique-secret"),
            "error must not echo secret: {msg}"
        );
        assert!(
            parse_project_git_remote(Some("https://github.com/acme/app.git"), Some("develop"))
                .unwrap()
                .is_some()
        );
        assert!(
            parse_project_git_remote(Some("git@github.com:acme/app.git"), None)
                .unwrap()
                .is_some(),
            "scp-style git@ must stay valid"
        );
        assert!(
            parse_project_git_remote(Some("ssh://git@github.com/acme/app.git"), None)
                .unwrap()
                .is_some(),
            "ssh://git@ must stay valid"
        );
    }

    #[test]
    fn rejects_invalid_git_ref() {
        let url = Some("https://example.com/r.git");
        assert!(err_text(url, Some("has space")).contains("git_ref"));
        assert!(err_text(url, Some("-bad")).contains("git_ref"));
        assert!(err_text(url, Some("feat/../x")).contains("git_ref"));
    }

    /// Docs §6.9 / §8.2: cookie and admin PATCH share these three outcomes.
    #[test]
    fn apply_git_remote_patch_empty_url_clears_git_ref_without_url_and_garbage_url_are_errors() {
        let url = "https://github.com/acme/app.git";
        let existing = bound(url, "develop");

        assert_eq!(
            apply_git_remote_patch(Some(&existing), "develop", Some(""), None).unwrap(),
            None,
            "empty URL must clear"
        );
        assert_eq!(
            apply_git_remote_patch(Some(&existing), "develop", Some("   "), None).unwrap(),
            None,
            "whitespace URL must clear"
        );
        assert_eq!(
            apply_git_remote_patch(Some(&existing), "develop", Some(""), Some("other")).unwrap(),
            None,
            "empty URL + git_ref is a clear, not an error"
        );

        let ref_only = apply_git_remote_patch(None, "main", None, Some("feature"))
            .expect_err("git_ref without URL must error");
        assert!(
            ref_only.to_string().contains("repo_url"),
            "ref-without-URL must mention repo_url: {ref_only}"
        );
        let empty_string_ref = apply_git_remote_patch(None, "main", None, Some(""))
            .expect_err("omitted URL + present git_ref field must error even when empty");
        assert!(
            empty_string_ref.to_string().contains("repo_url"),
            "empty-string git_ref without URL must mention repo_url: {empty_string_ref}"
        );

        for bad in [
            "not a url",
            "-e evil",
            "ftp://example.com/r.git",
            "https://example.com/has space.git",
            "javascript:alert(1)",
            "https://user:unique-secret@example.test/acme.git",
            "http://token:x-oauth-basic@github.com/acme/app.git",
        ] {
            let err =
                apply_git_remote_patch(Some(&existing), "develop", Some(bad), None).expect_err(bad);
            assert!(
                err.to_string().contains("repo_url"),
                "garbage {bad:?} must mention repo_url: {err}"
            );
            assert!(
                !err.to_string().contains("unique-secret")
                    && !err.to_string().contains("x-oauth-basic"),
                "error must not echo credentials: {err}"
            );
        }

        let updated = apply_git_remote_patch(Some(&existing), "develop", None, Some("feature"))
            .unwrap()
            .expect("git_ref with existing URL updates the ref");
        assert_eq!(updated.url, url);
        assert_eq!(updated.git_ref, "feature");

        let kept = apply_git_remote_patch(Some(&existing), "develop", None, None)
            .unwrap()
            .expect("omitted fields keep the binding");
        assert_eq!(kept, existing);

        let set = apply_git_remote_patch(None, "main", Some(url), Some("develop"))
            .unwrap()
            .expect("non-empty valid URL binds");
        assert_eq!(set, bound(url, "develop"));
    }

    #[test]
    fn cookie_and_admin_patch_share_apply_git_remote_patch() {
        let admin = include_str!("../api/handlers.rs");
        let admin_fn = admin
            .split("pub async fn patch_config")
            .nth(1)
            .expect("admin patch_config")
            .split("pub async fn ")
            .next()
            .expect("admin patch_config body");
        assert!(
            admin_fn.contains("apply_git_remote_patch"),
            "admin PATCH /v1/projects/{{id}}/config must call apply_git_remote_patch (Docs §8.2 same semantics as cookie)"
        );
        assert!(
            !admin_fn.contains("git_ref requires repo_url on this project"),
            "admin PATCH must not re-implement the ref-without-URL branch"
        );
    }
}
