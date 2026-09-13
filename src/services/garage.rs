use super::Service;
use crate::config::GarageConfig;
use crate::context::WorkspaceContext;
use crate::docker::DockerCompose;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::time::Duration;

const REGION: &str = "garage";

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
            if garage(&["bucket", "info", bucket]).is_err() {
                println!("{} Creating bucket '{}'...", "[garage]".blue().bold(), bucket.cyan());
                garage(&["bucket", "create", bucket])?;
            }
            garage(&["bucket", "allow", bucket, "--read", "--write", "--owner", "--key", &self.config.access_key])?;
        }

        // 2. Website hosting
        for bucket in &self.config.website_buckets {
            println!("{} Enabling website hosting for bucket '{}'...", "[garage]".blue().bold(), bucket.cyan());
            garage(&["bucket", "website", "--allow", bucket])?;
        }

        // 3. CORS via the S3 API (not exposed by the Garage CLI)
        let host = format!("127.0.0.1:{}", ctx.port_allocations["garage"]);
        for bucket in &buckets {
            if let Err(e) = put_bucket_cors(&host, bucket, &self.config.access_key, &self.config.secret_key) {
                eprintln!("{} Warning: failed to set CORS on bucket '{}': {:#}", "[garage]".yellow().bold(), bucket, e);
            }
        }

        Ok(())
    }
}

fn put_bucket_cors(host: &str, bucket: &str, access_key: &str, secret_key: &str) -> Result<()> {
    let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let uri = format!("/{bucket}");
    let request = S3Request { method: "PUT", host, uri: &uri, query: "cors=", payload: CORS_XML.as_bytes() };
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
fn sign_v4(req: &S3Request, access_key: &str, secret_key: &str, region: &str, amz_date: &str) -> SignedRequest {
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
mod tests {
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
