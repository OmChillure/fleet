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
    let s = raw.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("main");
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
        return Err(GitRemoteError::Invalid("repo_url must not start with '-'".into()));
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
    Ok(())
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
        return Err(GitRemoteError::Invalid("git_ref is not a valid git ref".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_url_clears_binding() {
        assert_eq!(parse_project_git_remote(Some("  "), Some("main")).unwrap(), None);
        assert_eq!(parse_project_git_remote(None, Some("develop")).unwrap(), None);
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
        assert!(parse_project_git_remote(Some("not a url"), None).is_err());
        assert!(parse_project_git_remote(Some("-e evil"), None).is_err());
    }
}
