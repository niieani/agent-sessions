//! Repository keys: the normalized `owner/repo` identity of a recorded remote URL.

/// Host aliases and nested server namespaces do not participate in Project identity.
pub(crate) fn repository_key(url: &str) -> Option<String> {
    if url.is_empty()
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
        || url.contains(['?', '#', '\\'])
    {
        return None;
    }
    let path = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.is_empty() {
            return None;
        }
        let (host, path) = rest.split_once('/')?;
        if host.is_empty() {
            return None;
        }
        path
    } else if let Some((host, path)) = url.split_once(':') {
        if host.is_empty() || host.contains('/') {
            return None;
        }
        path
    } else {
        url
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments = path.split('/').collect::<Vec<_>>();
    if segments.len() < 2
        || segments
            .iter()
            .any(|s| s.is_empty() || matches!(*s, "." | "..") || s.contains(':'))
    {
        return None;
    }
    Some(segments[segments.len() - 2..].join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repository_urls_normalize_without_host_or_transport() {
        for url in [
            "git@example.github.com:example-org/assistant.git",
            "git@github.com:example-org/assistant.git",
            "https://github.com/example-org/assistant.git",
            "ssh://git@github.com/example-org/assistant",
            "ssh://git@github.com:2222/example-org/assistant.git///",
            "https://gitlab.com/group/example-org/assistant.git",
            "example-org/assistant",
        ] {
            assert_eq!(
                repository_key(url).as_deref(),
                Some("example-org/assistant"),
                "{url}"
            );
        }
        for url in [
            "",
            "repo",
            "https://github.com/repo",
            "git@host:repo",
            "https://host/owner//repo",
            "https://host/owner/.git",
            "https://host/../repo",
            "https://host/owner/repo?x=y",
        ] {
            assert_eq!(repository_key(url), None, "{url}");
        }
    }
}
