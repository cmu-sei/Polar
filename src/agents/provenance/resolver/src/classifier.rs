use cassini_client::TcpClientMessage;
use polar::{NormalizedComponent, NormalizedSbom, ProvenanceEvent, PROVENANCE_LINKER_TOPIC};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use reqwest::Client as WebClient;
use rkyv::rancor;
use tracing::{debug, error, info, instrument, trace};

pub struct ArtifactClassifier;

pub struct ArtifactClassifierState {
    pub web_client: WebClient,
    pub tcp_client: ActorRef<TcpClientMessage>,
}

#[derive(Debug)]
pub enum ArtifactClassifierMsg {
    Classify {
        download_url: String,
        filename: String,
    },
}

impl ArtifactClassifier {
    fn parse_cyclonedx(bytes: &[u8]) -> Result<NormalizedSbom, ActorProcessingErr> {
        use cyclonedx_bom::prelude::*;

        debug!("Attempting to read data as cyclonedx json.");
        let bom = Bom::parse_from_json(bytes)
            .map_err(|e| ActorProcessingErr::from(format!("not a cyclonedx SBOM: {e:?}")))?;

        if bom.validate().passed() {
            let spec_version = bom.spec_version.to_string();

            debug!("Read SBOM. spec version: {spec_version}");

            if let Some(components) = bom.components {
                let components = components
                    .0
                    .into_iter()
                    .filter(|c| c.purl.is_some())
                    .map(|c| {
                        // TODO: we can do better than this I'm sure, especaily if we want more data
                        // we atleast need a PURL, so if this isn't present, we can move on
                        let purl = c.purl.unwrap();

                        NormalizedComponent {
                            name: c.name.to_string(),
                            version: c.version.unwrap_or("unknwon".into()).to_string(),
                            purl: purl.to_string(),
                        }
                    })
                    .collect::<Vec<_>>();

                Ok(NormalizedSbom {
                    format: polar::SbomFormat::CycloneDx,
                    spec_version,
                    components,
                })
            } else {
                return Err(ActorProcessingErr::from(
                    "cyclonedx SBOM contained zero components",
                ));
            }
        } else {
            return Err(ActorProcessingErr::from(
                "Failed to validate data as an SBOM",
            ));
        }
    }
}

#[instrument(name = "ArtifactClassifier.download_artifact", skip(client))]
async fn download_artifact(client: &WebClient, url: &str) -> Result<Vec<u8>, ActorProcessingErr> {
    debug!("Downloading artifact");
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| ActorProcessingErr::from(format!("download failed: {e:?}")))?;

    if !resp.status().is_success() {
        return Err(ActorProcessingErr::from(format!(
            "download returned status {}",
            resp.status()
        )));
    }

    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| ActorProcessingErr::from(format!("read body failed: {e:?}")))
}

#[async_trait]
impl Actor for ArtifactClassifier {
    type Msg = ArtifactClassifierMsg;
    type State = ArtifactClassifierState;
    type Arguments = ArtifactClassifierState;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(args)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ArtifactClassifierMsg::Classify {
                download_url,
                filename,
            } => {
                trace!("Received Classify directive.");
                let bytes = download_artifact(&state.web_client, &download_url).await?;

                let uid = polar::artifact_uid_from_bytes(&bytes);
                /*
                * TODO: Attempt to classify the artifact as one of some known possible value.
                It's possible for these values to be SBOMs in cyclonedx or spdx, potential test results/vulnerability reports, etc.
                */
                match Self::parse_cyclonedx(&bytes) {
                    Ok(sbom) => {
                        let event = ProvenanceEvent::SBOMResolved {
                            uid,
                            name: filename,
                            sbom,
                        };
                        let payload = rkyv::to_bytes::<rancor::Error>(&event)?;

                        state.tcp_client.cast(TcpClientMessage::Publish {
                            topic: PROVENANCE_LINKER_TOPIC.to_string(),
                            payload: payload.into(),
                        })?;
                    }
                    Err(e) => debug!("Couldn't parse as cycloneDx {e}"),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod classifier_tests {
    use super::*;
    use polar::SbomFormat;

    fn valid_cyclonedx() -> Vec<u8> {
        r#"
        {
          "bomFormat": "CycloneDX",
          "specVersion": "1.5",
          "version": 1,
          "components": [
            {
              "type": "library",
              "name": "serde",
              "version": "1.0.193",
              "purl": "pkg:cargo/serde@1.0.193"
            }
          ]
        }
        "#
        .as_bytes()
        .to_vec()
    }

    #[test]
    fn parses_valid_cyclonedx_sbom() {
        let sbom = ArtifactClassifier::parse_cyclonedx(&valid_cyclonedx()).expect("should parse");

        assert_eq!(sbom.format, SbomFormat::CycloneDx);
        assert_eq!(sbom.components.len(), 1);
        assert_eq!(sbom.components[0].name, "serde");
    }

    #[test]
    fn rejects_sbom_with_no_components() {
        let bytes = r#"
        {
          "bomFormat": "CycloneDX",
          "specVersion": "1.5",
          "version": 1
        }
        "#
        .as_bytes()
        .to_vec();

        let err = ArtifactClassifier::parse_cyclonedx(&bytes).unwrap_err();
        assert!(err.to_string().contains("zero components"));
    }

    #[test]
    fn rejects_components_without_purl() {
        let bytes = r#"
        {
          "bomFormat": "CycloneDX",
          "specVersion": "1.5",
          "version": 1,
          "components": [
            { "name": "serde", "version": "1.0.0" ,"type": "library" }
          ]
        }
        "#
        .as_bytes()
        .to_vec();

        let sbom = ArtifactClassifier::parse_cyclonedx(&bytes).unwrap();
        assert!(sbom.components.is_empty());
    }

    #[test]
    fn rejects_non_cyclonedx_json() {
        let bytes = r#"{ "hello": "world" }"#.as_bytes().to_vec();
        assert!(ArtifactClassifier::parse_cyclonedx(&bytes).is_err());
    }

    #[test]
    fn rejects_invalid_json() {
        let bytes = b"{ not json }".to_vec();
        assert!(ArtifactClassifier::parse_cyclonedx(&bytes).is_err());
    }
}
