# **Secure Attestation Protocol for Containerized Services**

## **1. Overview**
This protocol ensures that only **legitimate, running containerized services** can obtain short-lived PKI certificates by verifying:
- The container originates from a **trusted build**.
- The attestation request **comes from an actual running instance**.
- The request is **fresh and unique** to prevent replay attacks.
- The attestation service can **verify the runtime environment** via SSH.
- **Rate-limiting and logging mechanisms** detect and prevent abuse.

## **2. Security Mechanisms**
| Mechanism | Purpose |
|-----------|---------|
| **Challenge (Nonce)** | Prevents replay attacks. |
| **Ephemeral Secret** | Binds attestation to a single running instance. |
| **HMAC Signatures** | Prevents request tampering and ensures authenticity. |
| **Runtime Introspection** | Verifies the container actually exists. |
| **Rate Limiting** | Prevents excessive requests or brute force attempts. |
| **Logging & Anomaly Detection** | Detects repeated failures or suspicious behavior. |

## **3. Protocol Steps**

### **Step 1: Service Requests a Challenge**
The service first obtains a one-time challenge from the attestation server.

```sh
curl -X POST https://attestation-service/challenge \
    -d '{ "build_hash": "abcdef123456", "host_id": "host-001" }'
```

**Attestation Service:**
```sh
CHALLENGE=$(openssl rand -hex 16) # Generate 128-bit random nonce
redis-cli SETEX "challenge:$CHALLENGE" 60 "valid" # Store with 60s expiry
```

### **Step 2: Service Requests an Ephemeral Secret**
```sh
curl -X POST https://attestation-service/ephemeral-secret \
    -d '{ "build_hash": "abcdef123456", "host_id": "host-001", "challenge": "abcd1234" }'
```

**Attestation Service Derives the Ephemeral Secret:**
```sh
SECRET_KEY=$(echo -n "INSTANCE_ID:$CHALLENGE" | openssl dgst -sha256 -hmac "$MASTER_SECRET" | awk '{print $2}')
redis-cli SETEX "secret:$INSTANCE_ID" 300 "$SECRET_KEY" # Store for 5 minutes
```

### **Step 3: Service Computes Signed Attestation Request**
```sh
SIGNATURE=$(echo -n "abcdef123456:RUNTIME_ID:abcd1234" | openssl dgst -sha256 -hmac "$SECRET_KEY" | awk '{print $2}')
```
```sh
curl -X POST https://attestation-service/verify \
    -d '{ "build_hash": "abcdef123456", "runtime_id": "container-xyz", "host_id": "host-001", "challenge": "abcd1234", "signature": "hmac_value" }'
```

### **Step 4: Attestation Service Validates Request**
```sh
EXPECTED_SECRET=$(echo -n "INSTANCE_ID:$CHALLENGE" | openssl dgst -sha256 -hmac "$MASTER_SECRET" | awk '{print $2}')
EXPECTED_HMAC=$(echo -n "abcdef123456:RUNTIME_ID:abcd1234" | openssl dgst -sha256 -hmac "$EXPECTED_SECRET" | awk '{print $2}')
```
```sh
if [ "$EXPECTED_HMAC" != "$RECEIVED_HMAC" ]; then exit 1; fi
```

### **Step 5: Attestation Service Validates Runtime Environment**
**SSH into Host and Verify Container:**
```sh
ssh root@host-001 "docker ps --filter id=container-xyz --format '{{.ID}}: {{.Image}}'"
```

### **Step 6: Issue Certificate**
If the container exists and matches the attestation request:
```sh
step ca certificate "my-service.example.com" cert.pem key.pem \
    --provisioner "attestation-service" --ca-url "https://ca.example.com"
```

## **4. Additional Security Enhancements**

### **Rate Limiting**
```sh
redis-cli INCR "rate:$INSTANCE_ID"
redis-cli EXPIRE "rate:$INSTANCE_ID" 60 # Track per minute
if [ $(redis-cli GET "rate:$INSTANCE_ID") -gt 5 ]; then exit 1; fi
```

### **Logging & Anomaly Detection**
```sh
echo "[$(date)] Failed attestation for INSTANCE_ID" >> /var/log/attestation.log
awk 'NR>5' /var/log/attestation.log | mail -s "Possible Attack Detected" security@example.com
```

## **5. Conclusion**
This protocol ensures that **only trusted, actively running containers** receive short-lived PKI certificates. With **ephemeral secrets, challenge-response mechanisms, rate limiting, and logging**, it is resistant to:
- **Replay attacks**
- **Impersonation attempts**
- **Unauthorized certificate issuance**

The integration of **runtime validation and SSH verification** makes it a highly **secure and resilient attestation mechanism** for cloud-native services.
