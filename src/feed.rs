//! Connection to the shred feed (gRPC `shredstream.ShredstreamProxy/SubscribeEntries`).
//!
//! The feed can be a Jito-style proxy you run yourself (`http://host:9999`) or a hosted
//! endpoint (`https://host:443`) that needs a token, sent as the `x-token` header.
use anyhow::{anyhow, Context, Result};
use jito_protos::shredstream::shredstream_proxy_client::ShredstreamProxyClient;
use tonic::metadata::MetadataValue;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Request, Status};

/// Adds the `x-token` header to every request when a token is set.
#[derive(Clone)]
pub struct TokenInterceptor(Option<MetadataValue<tonic::metadata::Ascii>>);

impl tonic::service::Interceptor for TokenInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(token) = &self.0 {
            req.metadata_mut().insert("x-token", token.clone());
        }
        Ok(req)
    }
}

pub type FeedClient = ShredstreamProxyClient<InterceptedService<Channel, TokenInterceptor>>;

/// The endpoint for `url`: TLS with the system's root certificates for `https://`, plain
/// for `http://`.
pub fn endpoint(url: &str) -> Result<Endpoint> {
    let url = url.trim();
    let mut endpoint = Endpoint::from_shared(url.to_string())
        .with_context(|| format!("bad SHREDSTREAM_URL {url:?}"))?;
    if url.starts_with("https://") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_enabled_roots())
            .context("could not set up TLS for the shred feed")?;
    } else if !url.starts_with("http://") {
        return Err(anyhow!(
            "SHREDSTREAM_URL must start with http:// or https://, got {url:?}"
        ));
    }
    Ok(endpoint)
}

/// The header value for a token, rejecting characters a header cannot carry.
pub fn token_value(token: &str) -> Result<MetadataValue<tonic::metadata::Ascii>> {
    token
        .parse()
        .map_err(|_| anyhow!("SHREDSTREAM_TOKEN has characters that cannot be sent as a header"))
}

pub async fn connect(url: &str, token: Option<&str>) -> Result<FeedClient> {
    // TLS needs a process-wide crypto provider; another thread may install one between the
    // check and here, which is fine.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let token = token.map(token_value).transpose()?;
    let channel = endpoint(url)?.connect().await?;
    Ok(ShredstreamProxyClient::with_interceptor(
        channel,
        TokenInterceptor(token),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::service::Interceptor;

    #[tokio::test]
    async fn http_and_https_endpoints_are_accepted_and_others_are_not() {
        assert!(endpoint("http://127.0.0.1:9999").is_ok());
        assert!(endpoint("https://sol-shredstream-x.example.net:443").is_ok());
        assert!(endpoint(" http://127.0.0.1:9999 ").is_ok());
        assert!(endpoint("127.0.0.1:9999").is_err());
        assert!(endpoint("ftp://host").is_err());
        assert!(endpoint("").is_err());
    }

    #[test]
    fn the_token_goes_in_the_x_token_header() {
        let mut with = TokenInterceptor(Some(token_value("abc123").unwrap()));
        let req = with.call(Request::new(())).unwrap();
        assert_eq!(req.metadata().get("x-token").unwrap(), "abc123");

        let mut without = TokenInterceptor(None);
        let req = without.call(Request::new(())).unwrap();
        assert!(req.metadata().get("x-token").is_none());

        assert!(token_value("bad\nvalue").is_err());
    }
}
