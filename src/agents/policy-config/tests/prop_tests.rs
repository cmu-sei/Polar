#[cfg(test)]
mod prop_tests {
    use super::*;
    use git2::{Repository, Signature};
    use proptest::prelude::*;
    use tempfile::TempDir;

    fn init_repo_with_commit() -> Repository {
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
        repo
    }

    proptest! {
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
            prop_assert!(url.starts_with("https://"));
            prop_assert!(url.contains(&user));
            prop_assert!(url.contains(&token));
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
            prop_assert!(!twice.matches(&format!("{user}:{token}@{user}:{token}@")).any(|_| true));
        }

        #[test]
        fn has_local_changes_invariant_after_clean_commit(
            message in ".*"
        ) {
            let repo = init_repo_with_commit();
            prop_assert_eq!(has_local_changes(&repo).unwrap(), false);
        }

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
            prop_assert_eq!(has_local_changes(&repo).unwrap(), true);
        }

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

            repo.branch("main", &repo.find_commit(c).unwrap(), true).unwrap();
            repo.reference("refs/remotes/origin/main", c, true, "").unwrap();

            for k in 0..n {
                std::fs::write(&f, format!("commit {k}")).unwrap();
                let mut idx = repo.index().unwrap();
                idx.add_path(Path::new("file.txt")).unwrap();
                let tid = idx.write_tree().unwrap();
                let tree = repo.find_tree(tid).unwrap();
                let parent = repo.head().unwrap().peel_to_commit().unwrap();
                repo.commit(Some("HEAD"), &sig, &sig, &format!("c{k}"), &tree, &[&parent]).unwrap();
            }

            // n>0 means HEAD diverged
            prop_assert_eq!(has_diverged_commits(&repo).unwrap(), n > 0);
        }
    }
}
