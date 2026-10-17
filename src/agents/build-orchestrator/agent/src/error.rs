use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("upload failed for key {key}: {reason}")]
    UploadFailed { key: String, reason: String },

    #[error("object not found: {0}")]
    NotFound(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("s3 error: {0}")]
    S3(String),
}
