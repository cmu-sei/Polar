{ apiVersion = "v1"
, kind = "PersistentVolumeClaim"
, metadata = { name = "polar-db-logs", namespace = "polar-db" }
, spec =
  { accessModes = [ "ReadWriteOnce" ]
  , resources.requests.storage = "50Gi"
  , storageClassName = "standard"
  }
}
