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
    fn scp_style_and_ssh_git_at_urls_stay_valid() {
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
    fn https_username_only_userinfo_is_rejected() {
        parse_project_git_remote(Some("https://git@github.com/acme/app.git"), None)
            .expect_err("https://git@ userinfo must not persist");
    }

    #[test]
    fn https_at_in_path_or_query_is_not_userinfo() {
        let path = parse_project_git_remote(Some("https://example.test/acme@org.git"), None)
            .unwrap()
            .unwrap();
        assert_eq!(path.url, "https://example.test/acme@org.git");
        let query = parse_project_git_remote(Some("https://example.test/r.git?x=a@b"), None)
            .unwrap()
            .unwrap();
        assert_eq!(query.url, "https://example.test/r.git?x=a@b");
    }

    #[test]
    fn http_userinfo_with_port_rejected_without_echo() {
        let msg = err_text(
            Some("https://user:unique-secret@example.test:443/acme.git"),
            None,
        );
        assert!(
            !msg.contains("unique-secret"),
            "error must not echo secret: {msg}"
        );
    }

    #[test]
    fn rejects_invalid_git_ref() {
        let url = Some("https://example.com/r.git");
        assert!(err_text(url, Some("has space")).contains("git_ref"));
        assert!(err_text(url, Some("-bad")).contains("git_ref"));
        assert!(err_text(url, Some("feat/../x")).contains("git_ref"));
    }

    #[test]
    fn apply_git_remote_patch_empty_url_clears() {
        let existing = bound("https://github.com/acme/app.git", "develop");
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
    }

    #[test]
    fn apply_git_remote_patch_git_ref_without_url_is_error() {
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
    }

    #[test]
    fn apply_git_remote_patch_garbage_url_is_error() {
        let existing = bound("https://github.com/acme/app.git", "develop");
        let err = apply_git_remote_patch(Some(&existing), "develop", Some("not a url"), None)
            .expect_err("garbage URL must error");
        assert!(
            err.to_string().contains("repo_url"),
            "garbage URL must mention repo_url: {err}"
        );
    }

    #[test]
    fn apply_git_remote_patch_omitted_url_updates_git_ref() {
        let url = "https://github.com/acme/app.git";
        let existing = bound(url, "develop");
        let updated = apply_git_remote_patch(Some(&existing), "develop", None, Some("feature"))
            .unwrap()
            .expect("git_ref with existing URL updates the ref");
        assert_eq!(updated.url, url);
        assert_eq!(updated.git_ref, "feature");
    }

    #[test]
    fn apply_git_remote_patch_omitted_fields_keep_binding() {
        let existing = bound("https://github.com/acme/app.git", "develop");
        let kept = apply_git_remote_patch(Some(&existing), "develop", None, None)
            .unwrap()
            .expect("omitted fields keep the binding");
        assert_eq!(kept, existing);
    }

    #[test]
    fn apply_git_remote_patch_valid_url_binds() {
        let url = "https://github.com/acme/app.git";
        let set = apply_git_remote_patch(None, "main", Some(url), Some("develop"))
            .unwrap()
            .expect("non-empty valid URL binds");
        assert_eq!(set, bound(url, "develop"));
    }

    #[test]
    fn apply_git_remote_patch_new_url_omitted_ref_keeps_existing_git_ref() {
        let existing = bound("https://github.com/acme/app.git", "develop");
        let new_url = "https://github.com/acme/other.git";
        let out = apply_git_remote_patch(Some(&existing), "develop", Some(new_url), None)
            .unwrap()
            .expect("new URL with omitted ref must keep existing git_ref");
        assert_eq!(out.url, new_url);
        assert_eq!(out.git_ref, "develop");
    }

    #[test]
    fn apply_git_remote_patch_omitted_url_invalid_git_ref_is_error() {
        let existing = bound("https://github.com/acme/app.git", "develop");
        let space = apply_git_remote_patch(Some(&existing), "develop", None, Some("has space"))
            .expect_err("whitespace git_ref must error");
        assert!(
            space.to_string().contains("git_ref"),
            "invalid git_ref must mention git_ref: {space}"
        );
        let leading_dash = apply_git_remote_patch(Some(&existing), "develop", None, Some("-bad"))
            .expect_err("leading-dash git_ref must error");
        assert!(
            leading_dash.to_string().contains("git_ref"),
            "invalid git_ref must mention git_ref: {leading_dash}"
        );
    }

    #[test]
    fn apply_git_remote_patch_omitted_url_blank_git_ref_defaults_to_main() {
        let url = "https://github.com/acme/app.git";
        let existing = bound(url, "develop");
        let out = apply_git_remote_patch(Some(&existing), "develop", None, Some("  "))
            .unwrap()
            .expect("blank git_ref must default to main");
        assert_eq!(out.url, url);
        assert_eq!(out.git_ref, "main");
    }
}
