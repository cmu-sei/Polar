source_commit: "$CI_COMMIT_SHA"
source_repo: "$CI_REPOSITORY_URL"
created_at: "$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
ci_pipeline:
  id: "$CI_PIPELINE_ID"
  url: "$CI_PIPELINE_URL"
environment: "$TARGET_ENV"