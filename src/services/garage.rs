use super::{Service, ServiceProvider};
use crate::config::ServicesConfig;
use crate::context::WorkspaceContext;
use crate::docker::DockerCompose;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use hmac::{Hmac, KeyInit, Mac};
use inquire::Text;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

const REGION: &str = "garage";

/// Local-only cluster secret for the single-node Garage instance.
pub const DEFAULT_GARAGE_RPC_SECRET: &str =
    "4425f5c26c5e11581d3223904324dcb5b5d5dfb14e5e7f35e38c595424f5f1e6";

const CORS_XML: &str = r#"<CORSConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <CORSRule>
    <AllowedOrigin>*</AllowedOrigin>
    <AllowedMethod>PUT</AllowedMethod>
    <AllowedMethod>GET</AllowedMethod>
    <AllowedMethod>HEAD</AllowedMethod>
    <AllowedMethod>POST</AllowedMethod>
    <AllowedHeader>*</AllowedHeader>
    <ExposeHeader>etag</ExposeHeader>
    <MaxAgeSeconds>3600</MaxAgeSeconds>
  </CORSRule>
</CORSConfiguration>"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GarageConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_garage_offset")]
    pub port_offset: u16,
    #[serde(default = "default_garage_web_offset")]
    pub web_port_offset: u16,
    #[serde(default = "default_garage_image")]
    pub image: String,
    pub access_key: String,
    pub secret_key: String,
    pub rpc_secret: Option<String>,
    #[serde(default)]
    pub buckets: Vec<String>,
    #[serde(default)]
    pub website_buckets: Vec<String>,
    #[serde(default = "default_web_root_domain")]
    pub website_root_domain: String,
}

impl GarageConfig {
    /// `buckets` followed by website buckets not already listed.
    pub fn all_buckets(&self) -> Vec<&str> {
        let mut all: Vec<&str> = Vec::new();
        for bucket in self.buckets.iter().chain(&self.website_buckets) {
            if !all.contains(&bucket.as_str()) {
                all.push(bucket);
            }
        }
        all
    }
}

fn default_true() -> bool {
    true
}

fn default_garage_offset() -> u16 {
    3
}

fn default_garage_web_offset() -> u16 {
    4
}

fn default_garage_image() -> String {
    "dxflrs/garage:v2.4.1".to_string()
}

fn default_web_root_domain() -> String {
    ".web.localhost".to_string()
}

pub struct GarageProvider;

impl ServiceProvider for GarageProvider {
    fn name(&self) -> &'static str {
        "garage"
    }

    fn init_label(&self) -> &'static str {
        "Garage S3 (local S3 + website hosting)"
    }

    fn reserved_names(&self) -> Vec<&'static str> {
        vec!["garage", "garage_web"]
    }

    fn port_offsets(&self, services: &ServicesConfig) -> Vec<(String, u16)> {
        if let Some(garage) = services.garage.as_ref().filter(|c| c.enabled) {
            vec![
                ("garage".to_string(), garage.port_offset),
                ("garage_web".to_string(), garage.web_port_offset),
            ]
        } else {
            vec![]
        }
    }

    fn prompt_init(
        &self,
        services: &mut ServicesConfig,
        project_name: &str,
        non_interactive: bool,
    ) -> Result<BTreeMap<String, String>> {
        let default_bucket = format!("{}-assets", project_name);
        let bucket_name = if non_interactive {
            default_bucket.clone()
        } else {
            Text::new("S3 bucket name:")
                .with_initial_value(&default_bucket)
                .with_help_message("Default bucket created and exposed for web hosting")
                .prompt()?
        };
        let final_bucket = non_empty(bucket_name).unwrap_or(default_bucket);

        services.garage = Some(GarageConfig {
            enabled: true,
            port_offset: 3,
            web_port_offset: 4,
            image: default_garage_image(),
            access_key: format!("{}-local-access-key", project_name),
            secret_key: format!("{}-local-secret-key-change-me", project_name),
            rpc_secret: None,
            buckets: vec![final_bucket.clone()],
            website_buckets: vec![final_bucket.clone()],
            website_root_domain: default_web_root_domain(),
        });

        let mut templates = BTreeMap::new();
        templates.insert(
            "S3_ENDPOINT".to_string(),
            "{{services.garage.endpoint}}".to_string(),
        );
        templates.insert(
            "S3_ACCESS_KEY_ID".to_string(),
            "{{services.garage.access_key}}".to_string(),
        );
        templates.insert(
            "S3_SECRET_ACCESS_KEY".to_string(),
            "{{services.garage.secret_key}}".to_string(),
        );
        templates.insert(
            "S3_REGION".to_string(),
            "{{services.garage.region}}".to_string(),
        );
        templates.insert("S3_BUCKET".to_string(), final_bucket.clone());
        templates.insert(
            "S3_PUBLIC_URL".to_string(),
            format!("http://{final_bucket}{{{{services.garage.website_root_domain}}}}:{{{{services.garage.web_port}}}}"),
        );
        Ok(templates)
    }

    fn write_auxiliary_files(&self, ctx: &WorkspaceContext, dir: &Path) -> Result<()> {
        if let Some(garage) = ctx.config.services.garage.as_ref().filter(|c| c.enabled) {
            let path = dir.join("garage.toml");
            fs::write(&path, garage_toml(&garage.website_root_domain))
                .with_context(|| format!("Failed to write garage.toml at {:?}", path))?;
        }
        Ok(())
    }

    fn contribute_compose(
        &self,
        ctx: &WorkspaceContext,
        services: &mut Map<String, Value>,
        volumes: &mut Map<String, Value>,
    ) {
        if let Some(garage) = ctx.config.services.garage.as_ref().filter(|c| c.enabled) {
            let port = |name: &str| ctx.port_allocations[name];
            let mut environment = json!({
                "GARAGE_DEFAULT_ACCESS_KEY": garage.access_key,
                "GARAGE_DEFAULT_SECRET_KEY": garage.secret_key,
                "GARAGE_RPC_SECRET": garage.rpc_secret.as_deref().unwrap_or(DEFAULT_GARAGE_RPC_SECRET),
            });
            let default_flag = match garage.all_buckets().first() {
                Some(bucket) => {
                    environment["GARAGE_DEFAULT_BUCKET"] = json!(bucket);
                    "--default-bucket"
                }
                None => "--default-access-key",
            };
            services.insert(
                "garage".into(),
                json!({
                    "image": garage.image,
                    "command": ["/garage", "server", "--single-node", default_flag],
                    "environment": environment,
                    "healthcheck": {
                        "test": ["CMD", "/garage", "status"],
                        "interval": "2s",
                        "timeout": "5s",
                        "retries": 30,
                    },
                    "ports": [format!("{}:3900", port("garage")), format!("{}:3902", port("garage_web"))],
                    "volumes": [
                        "garage-data:/var/lib/garage/data",
                        "garage-meta:/var/lib/garage/meta",
                        format!("{}:/etc/garage.toml:ro", ctx.igniter_dir().join("garage.toml").display()),
                    ],
                }),
            );
            volumes.insert("garage-data".into(), json!({}));
            volumes.insert("garage-meta".into(), json!({}));
        }
    }

    fn contribute_template_vars(
        &self,
        ctx: &WorkspaceContext,
        vars: &mut BTreeMap<String, String>,
    ) {
        if let Some(garage) = ctx.config.services.garage.as_ref().filter(|c| c.enabled) {
            let port = ctx.port_allocations["garage"];
            vars.insert("services.garage.port".into(), port.to_string());
            vars.insert(
                "services.garage.web_port".into(),
                ctx.port_allocations["garage_web"].to_string(),
            );
            vars.insert(
                "services.garage.endpoint".into(),
                format!("http://localhost:{port}"),
            );
            vars.insert(
                "services.garage.access_key".into(),
                garage.access_key.clone(),
            );
            vars.insert(
                "services.garage.secret_key".into(),
                garage.secret_key.clone(),
            );
            vars.insert("services.garage.region".into(), "garage".to_string());
            vars.insert(
                "services.garage.website_root_domain".into(),
                garage.website_root_domain.clone(),
            );
        }
    }

    fn get_active_service<'a>(&self, ctx: &'a WorkspaceContext) -> Option<Box<dyn Service + 'a>> {
        ctx.config
            .services
            .garage
            .as_ref()
            .filter(|c| c.enabled)
            .map(|g| {
                let s: Box<dyn Service + 'a> = Box::new(GarageService { config: g });
                s
            })
    }
}

pub struct GarageService<'a> {
    pub config: &'a GarageConfig,
}

impl Service for GarageService<'_> {
    fn name(&self) -> &str {
        "garage"
    }

    fn post_start(&self, ctx: &WorkspaceContext, compose: &DockerCompose<'_>) -> Result<()> {
        let garage = |args: &[&str]| {
            let mut full = vec!["/garage"];
            full.extend_from_slice(args);
            compose.exec_checked("garage", &full)
        };
        let buckets = self.config.all_buckets();

        // 1. Buckets & permissions (`bucket create` also registers the global alias used for website routing)
        for bucket in &buckets {
            match garage(&["bucket", "info", bucket]) {
                Ok(_) => {}
                Err(err) => {
                    let msg = err.to_string();
                    if msg.contains("NoSuchBucket") || msg.contains("Bucket not found") {
                        println!(
                            "{} Creating bucket '{}'...",
                            "[garage]".blue().bold(),
                            bucket.cyan()
                        );
                        garage(&["bucket", "create", bucket])?;
                    } else {
                        return Err(err)
                            .context(format!("Failed to query bucket '{bucket}' in garage"));
                    }
                }
            }
            garage(&[
                "bucket",
                "allow",
                bucket,
                "--read",
                "--write",
                "--owner",
                "--key",
                &self.config.access_key,
            ])?;
        }

        // 2. Website hosting
        for bucket in &self.config.website_buckets {
            println!(
                "{} Enabling website hosting for bucket '{}'...",
                "[garage]".blue().bold(),
                bucket.cyan()
            );
            garage(&["bucket", "website", "--allow", bucket])?;
        }

        // 3. CORS via the S3 API (not exposed by the Garage CLI)
        let host = format!("127.0.0.1:{}", ctx.port_allocations["garage"]);
        for bucket in &buckets {
            if let Err(e) = put_bucket_cors(
                &host,
                bucket,
                &self.config.access_key,
                &self.config.secret_key,
            ) {
                eprintln!(
                    "{} Warning: failed to set CORS on bucket '{}': {:#}",
                    "[garage]".yellow().bold(),
                    bucket,
                    e
                );
            }
        }

        Ok(())
    }
}

fn garage_toml(website_root_domain: &str) -> String {
    format!(
        r#"metadata_dir = "/var/lib/garage/meta"
data_dir = "/var/lib/garage/data"
db_engine = "sqlite"
replication_factor = 1
rpc_bind_addr = "[::]:3901"

[s3_api]
s3_region = "garage"
api_bind_addr = "[::]:3900"
root_domain = ".s3.garage.localhost"

[s3_web]
bind_addr = "[::]:3902"
root_domain = {}
index = "index.html"
"#,
        toml::Value::String(website_root_domain.to_string())
    )
}

fn non_empty(input: String) -> Option<String> {
    let trimmed = input.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn put_bucket_cors(host: &str, bucket: &str, access_key: &str, secret_key: &str) -> Result<()> {
    let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let uri = format!("/{bucket}");
    let request = S3Request {
        method: "PUT",
        host,
        uri: &uri,
        query: "cors=",
        payload: CORS_XML.as_bytes(),
    };
    let signed = sign_v4(&request, access_key, secret_key, REGION, &amz_date);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .put(format!("http://{host}{uri}?cors"))
        .header("x-amz-date", &amz_date)
        .header("x-amz-content-sha256", &signed.payload_hash)
        .header("authorization", &signed.authorization)
        .header("content-type", "application/xml")
        .send(CORS_XML)
        .context("PutBucketCors request failed")?;

    if !response.status().is_success() {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        bail!("PutBucketCors returned {}: {}", response.status(), body);
    }
    Ok(())
}

struct S3Request<'a> {
    method: &'a str,
    host: &'a str,
    uri: &'a str,
    /// Canonical query string, e.g. `cors=`
    query: &'a str,
    payload: &'a [u8],
}

struct SignedRequest {
    payload_hash: String,
    authorization: String,
}

/// AWS Signature V4 signing the `host`, `x-amz-content-sha256` and `x-amz-date` headers.
fn sign_v4(
    req: &S3Request,
    access_key: &str,
    secret_key: &str,
    region: &str,
    amz_date: &str,
) -> SignedRequest {
    let date_stamp = &amz_date[..8];
    let payload_hash = hex::encode(Sha256::digest(req.payload));
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";

    let canonical_request = format!(
        "{}\n{}\n{}\nhost:{}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n\n{signed_headers}\n{payload_hash}",
        req.method, req.uri, req.query, req.host
    );
    let scope = format!("{date_stamp}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let mut key = format!("AWS4{secret_key}").into_bytes();
    for part in [date_stamp, region, "s3", "aws4_request"] {
        key = hmac_sha256(&key, part.as_bytes());
    }
    let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));

    SignedRequest {
        payload_hash,
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
        ),
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// "GET Bucket Lifecycle" example from the AWS Signature V4 documentation.
    #[test]
    fn matches_aws_signature_v4_example() {
        let request = S3Request {
            method: "GET",
            host: "examplebucket.s3.amazonaws.com",
            uri: "/",
            query: "lifecycle=",
            payload: b"",
        };
        let signed = sign_v4(
            &request,
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "20130524T000000Z",
        );
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        );
    }
}
