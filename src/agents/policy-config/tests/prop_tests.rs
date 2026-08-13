#[cfg(test)]
mod prop_tests {
    use git2::{Repository, Signature};
    use policy_config::*;
    use proptest::prelude::*;
    use std::path::Path;
    use tempfile::TempDir;
    use urlencoding::encode; // <-- add this for encoded comparisons

    // --- Fix #3: Keep TempDir alive through return ---
    fn init_repo_with_commit() -> (TempDir, Repository) {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();
        let sig = Signature::now("Tester", "test@example.com").unwrap();
        let path = tmp.path().join("file.txt");
        std::fs::write(&path, "initial").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        drop(tree);
        (tmp, repo)
    }

    proptest! {

        // --- Fix #1: Compare encoded user/token ---
        #[test]
        fn url_formatting_handles_any_valid_combo(
            user in "\\w{1,10}",
            token in "[A-Za-z0-9_\\-]{5,40}",
            domain in "(github|gitlab)\\.com",
            org in "[a-zA-Z0-9_\\-]{1,15}",
            repo in "[a-zA-Z0-9_\\-]{1,15}"
        ) {
            let raw = format!("https://{domain}/{org}/{repo}.git");
            let url = get_auth_repo_url(&raw, &user, &token);

            let encoded_user = encode(&user);
            let encoded_token = encode(&token);

            prop_assert!(url.starts_with("https://"));
            prop_assert!(url.contains(encoded_user.as_ref()));
            prop_assert!(url.contains(encoded_token.as_ref()));
            prop_assert!(url.contains(&domain));
        }

        #[test]
        fn url_formatting_is_idempotent_when_called_twice(
            user in "\\w{1,10}",
            token in "[A-Za-z0-9_\\-]{5,40}",
            domain in "(github|gitlab)\\.com",
            org in "[a-zA-Z0-9_\\-]{1,15}",
            repo in "[a-zA-Z0-9_\\-]{1,15}"
        ) {
            let raw = format!("https://{domain}/{org}/{repo}.git");
            let once = get_auth_repo_url(&raw, &user, &token);
            let twice = get_auth_repo_url(&once, &user, &token);

            // second call shouldn't double-wrap credentials
            let pattern = format!("{}:{}@{}:{}@", user, token, user, token);
            prop_assert!(!twice.matches(&pattern).any(|_| true));
        }

        // --- Fix #3: Return tuple so TempDir lives ---
        #[test]
        fn has_local_changes_invariant_after_clean_commit(
            _message in ".*"
        ) {
            let (_tmp, repo) = init_repo_with_commit(); // <-- keep tmp alive
            prop_assert_eq!(has_local_changes(&repo).unwrap(), false);
        }

        // --- Fix #4: Sleep + refresh index for mtimes ---
        #[test]
        fn has_local_changes_true_after_modification(
            new_content in ".*"
        ) {
            let tmp = TempDir::new().unwrap();
            let repo = Repository::init(tmp.path()).unwrap();
            let sig = Signature::now("Tester", "test@example.com").unwrap();
            let file = tmp.path().join("file.txt");
            std::fs::write(&file, "data").unwrap();
            let mut idx = repo.index().unwrap();
            idx.add_path(Path::new("file.txt")).unwrap();
            let tree_id = idx.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();

            // modify file arbitrarily
            std::fs::write(&file, new_content).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
            repo.index().unwrap().read(true).unwrap();

            prop_assert_eq!(has_local_changes(&repo).unwrap(), true);
        }

        // --- Fix #2: Detach HEAD before setting remote ---
        #[test]
        fn divergence_detected_when_heads_differ(n in 1u8..5u8) {
            let tmp = TempDir::new().unwrap();
            let repo = Repository::init(tmp.path()).unwrap();
            let sig = Signature::now("Tester", "t@x.com").unwrap();
            let f = tmp.path().join("file.txt");
            std::fs::write(&f, "x").unwrap();
            let mut i = repo.index().unwrap();
            i.add_path(Path::new("file.txt")).unwrap();
            let t = repo.find_tree(i.write_tree().unwrap()).unwrap();
            let c = repo.commit(Some("HEAD"), &sig, &sig, "init", &t, &[]).unwrap();

            repo.set_head_detached(c).unwrap();
            repo.reference("refs/remotes/origin/main", c, false, "simulate remote base").unwrap();
            repo.branch("main", &repo.find_commit(c).unwrap(), true).unwrap();
            repo.set_head("refs/heads/main").unwrap();

            // Add dummy remote and tracking configuration so branch.upstream() works
            repo.remote("origin", "https://example.com/dummy.git").unwrap();
            let mut cfg = repo.config().unwrap();
            cfg.set_str("branch.main.remote", "origin").unwrap();
            cfg.set_str("branch.main.merge", "refs/heads/main").unwrap();

            for k in 0..n {
                std::fs::write(&f, format!("commit {k}")).unwrap();
                let mut idx = repo.index().unwrap();
                idx.add_path(Path::new("file.txt")).unwrap();
                let tid = idx.write_tree().unwrap();
                let tree = repo.find_tree(tid).unwrap();
                let parent = repo.head().unwrap().peel_to_commit().unwrap();
                repo.commit(Some("HEAD"), &sig, &sig, &format!("c{k}"), &tree, &[&parent]).unwrap();
            }

            prop_assert_eq!(has_diverged_commits(&repo).unwrap(), n > 0);
        }
    }
}
