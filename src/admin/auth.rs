//! Keycloak access token validation for the admin API.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header,
    jwk::{JwkSet, PublicKeyUse},
};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};

/// JWKS refreshes are attempted at most this often, successful or not.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
/// Cached keys are refreshed when they are older than this.
const MAX_KEY_AGE: Duration = Duration::from_secs(3600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct AdminAuthConfig {
    pub issuer: String,
    pub client_id: String,
    pub group: String,
}

/// The authenticated admin, taken from the token claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminUser {
    pub username: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// Missing or invalid token: 401.
    Unauthorized(String),
    /// Valid token without the admin group: 403.
    Forbidden,
    /// The signing keys could not be loaded: 503.
    Unavailable(String),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Default for Audience {
    fn default() -> Self {
        Audience::Many(Vec::new())
    }
}

impl Audience {
    fn contains(&self, value: &str) -> bool {
        match self {
            Audience::One(audience) => audience == value,
            Audience::Many(audiences) => audiences.iter().any(|audience| audience == value),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Claims {
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    aud: Audience,
}

#[derive(Default)]
struct KeyCache {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Option<Instant>,
    /// Last refresh attempt, also a failed one.
    attempted_at: Option<Instant>,
}

pub struct AdminAuth {
    config: AdminAuthConfig,
    jwks_url: String,
    client: reqwest::Client,
    cache: RwLock<KeyCache>,
    refresh_lock: Mutex<()>,
    min_refresh_interval: Duration,
}

impl AdminAuth {
    pub fn new(config: AdminAuthConfig) -> Self {
        let jwks_url = format!(
            "{}/protocol/openid-connect/certs",
            config.issuer.trim_end_matches('/')
        );
        Self {
            config,
            jwks_url,
            client: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .expect("JWKS HTTP client should build"),
            cache: RwLock::new(KeyCache::default()),
            refresh_lock: Mutex::new(()),
            min_refresh_interval: MIN_REFRESH_INTERVAL,
        }
    }

    #[cfg(test)]
    pub fn with_min_refresh_interval(mut self, interval: Duration) -> Self {
        self.min_refresh_interval = interval;
        self
    }

    /// Validates the `Authorization` header value and returns the admin user.
    pub async fn authenticate(&self, authorization: Option<&str>) -> Result<AdminUser, AuthError> {
        let token = authorization
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AuthError::Unauthorized("Missing bearer token".to_string()))?;

        let header = decode_header(token)
            .map_err(|e| AuthError::Unauthorized(format!("Invalid token: {e}")))?;
        if header.alg != Algorithm::RS256 {
            return Err(AuthError::Unauthorized(
                "Invalid token algorithm".to_string(),
            ));
        }
        let kid = header
            .kid
            .ok_or_else(|| AuthError::Unauthorized("Token has no key ID".to_string()))?;
        let key = self.key(&kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_required_spec_claims(&["exp", "iss"]);
        validation.validate_aud = false;
        validation.validate_nbf = true;
        validation.leeway = 30;
        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|e| AuthError::Unauthorized(format!("Invalid token: {e}")))?
            .claims;

        if claims.azp.as_deref() != Some(self.config.client_id.as_str())
            && !claims.aud.contains(&self.config.client_id)
        {
            return Err(AuthError::Unauthorized(
                "Token was issued for another client".to_string(),
            ));
        }
        if !claims
            .groups
            .iter()
            .any(|group| group == &self.config.group)
        {
            return Err(AuthError::Forbidden);
        }

        Ok(AdminUser {
            username: claims
                .preferred_username
                .or(claims.sub)
                .unwrap_or_else(|| "unknown".to_string()),
            name: claims.name,
            email: claims.email,
            groups: claims.groups,
        })
    }

    async fn key(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        let (cached, may_refresh) = {
            let cache = self.cache.read().await;
            let fresh = cache
                .fetched_at
                .is_some_and(|fetched_at| fetched_at.elapsed() < MAX_KEY_AGE);
            let cached = cache.keys.get(kid).cloned();
            if fresh && let Some(key) = cached {
                return Ok(key);
            }
            (cached, self.may_attempt_refresh(&cache))
        };

        if may_refresh {
            // Never wait for a refresh in progress: use the cached key or fail fast.
            match self.refresh_lock.try_lock() {
                Ok(_refresh) => self.refresh().await,
                Err(_) if cached.is_some() => {}
                Err(_) => {
                    return Err(AuthError::Unavailable(
                        "Token signing keys are being refreshed, try again".to_string(),
                    ));
                }
            }
        }

        // Keep using known keys while Keycloak is unreachable.
        let cache = self.cache.read().await;
        match cache.keys.get(kid) {
            Some(key) => Ok(key.clone()),
            None if cache.keys.is_empty() => Err(AuthError::Unavailable(
                "Token signing keys are not available".to_string(),
            )),
            None => Err(AuthError::Unauthorized("Unknown token key".to_string())),
        }
    }

    fn may_attempt_refresh(&self, cache: &KeyCache) -> bool {
        cache
            .attempted_at
            .is_none_or(|attempted_at| attempted_at.elapsed() >= self.min_refresh_interval)
    }

    /// Fetches the keys unless another attempt was made within the minimum
    /// interval. Callers must hold `refresh_lock`.
    async fn refresh(&self) {
        {
            let mut cache = self.cache.write().await;
            if !self.may_attempt_refresh(&cache) {
                return;
            }
            cache.attempted_at = Some(Instant::now());
        }
        match self.fetch_keys().await {
            Ok(keys) => {
                let mut cache = self.cache.write().await;
                cache.keys = keys;
                cache.fetched_at = Some(Instant::now());
            }
            Err(error) => {
                tracing::warn!(%error, url = %self.jwks_url, "jwks_fetch_failed");
            }
        }
    }

    async fn fetch_keys(&self) -> Result<HashMap<String, DecodingKey>> {
        let jwks = self
            .client
            .get(&self.jwks_url)
            .send()
            .await?
            .error_for_status()?
            .json::<JwkSet>()
            .await?;
        let mut keys = HashMap::new();
        for jwk in &jwks.keys {
            if jwk.common.public_key_use == Some(PublicKeyUse::Encryption) {
                continue;
            }
            let Some(kid) = jwk.common.key_id.clone() else {
                continue;
            };
            if let Ok(key) = DecodingKey::from_jwk(jwk) {
                keys.insert(kid, key);
            }
        }
        if keys.is_empty() {
            return Err(anyhow!("JWKS contains no usable signing keys"));
        }
        Ok(keys)
    }
}

#[cfg(test)]
pub mod test_tokens {
    use super::super::test_keys::*;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::{Value, json};

    pub const TEST_KID: &str = "test-key";
    pub const TEST_CLIENT: &str = "bakta-admin";
    pub const TEST_GROUP: &str = "bakta-admin";

    pub fn jwks() -> Value {
        json!({"keys": [
            {"kid": "enc-key", "kty": "RSA", "alg": "RSA-OAEP", "use": "enc",
             "n": TEST_RSA_MODULUS, "e": TEST_RSA_EXPONENT},
            {"kid": TEST_KID, "kty": "RSA", "alg": "RS256", "use": "sig",
             "n": TEST_RSA_MODULUS, "e": TEST_RSA_EXPONENT}
        ]})
    }

    /// Claims of a valid admin token for `issuer`.
    pub fn claims(issuer: &str) -> Value {
        let now = chrono::Utc::now().timestamp();
        json!({
            "iss": issuer, "exp": now + 300, "nbf": now - 10, "iat": now - 10,
            "azp": TEST_CLIENT, "aud": "account", "sub": "user-id",
            "preferred_username": "jdoe", "name": "Jane Doe", "email": "jdoe@example.org",
            "groups": [TEST_GROUP, "other"]
        })
    }

    pub fn token_with_kid(claims: &Value, kid: &str) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        let key = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_KEY.as_bytes()).unwrap();
        encode(&header, claims, &key).unwrap()
    }

    pub fn token(claims: &Value) -> String {
        token_with_kid(claims, TEST_KID)
    }
}

#[cfg(test)]
mod tests {
    use super::{test_tokens::*, *};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    async fn setup() -> (MockServer, AdminAuth, String) {
        let keycloak = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/realms/bakta/protocol/openid-connect/certs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks()))
            .mount(&keycloak)
            .await;
        let issuer = format!("{}/realms/bakta", keycloak.uri());
        let auth = AdminAuth::new(AdminAuthConfig {
            issuer: issuer.clone(),
            client_id: TEST_CLIENT.to_string(),
            group: TEST_GROUP.to_string(),
        });
        (keycloak, auth, issuer)
    }

    async fn check(auth: &AdminAuth, claims: &serde_json::Value) -> Result<AdminUser, AuthError> {
        auth.authenticate(Some(&format!("Bearer {}", token(claims))))
            .await
    }

    #[tokio::test]
    async fn test_valid_token_returns_the_user_and_keys_are_cached() {
        let (keycloak, auth, issuer) = setup().await;
        let user = check(&auth, &claims(&issuer)).await.unwrap();
        assert_eq!(user.username, "jdoe");
        assert_eq!(user.name.as_deref(), Some("Jane Doe"));
        assert_eq!(user.email.as_deref(), Some("jdoe@example.org"));
        assert_eq!(user.groups, ["bakta-admin", "other"]);

        check(&auth, &claims(&issuer)).await.unwrap();
        assert_eq!(keycloak.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_client_may_be_in_the_audience_instead_of_azp() {
        let (_keycloak, auth, issuer) = setup().await;
        let mut claims = claims(&issuer);
        claims["azp"] = "other-client".into();
        claims["aud"] = serde_json::json!(["account", TEST_CLIENT]);
        assert!(check(&auth, &claims).await.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_tokens_are_rejected() {
        let (_keycloak, auth, issuer) = setup().await;
        let now = chrono::Utc::now().timestamp();

        let mut expired = claims(&issuer);
        expired["exp"] = (now - 3600).into();
        let mut not_yet_valid = claims(&issuer);
        not_yet_valid["nbf"] = (now + 3600).into();
        let mut wrong_issuer = claims(&issuer);
        wrong_issuer["iss"] = "https://evil.example/realms/bakta".into();
        let mut wrong_client = claims(&issuer);
        wrong_client["azp"] = "other-client".into();

        for (name, claims) in [
            ("expired", expired),
            ("not yet valid", not_yet_valid),
            ("wrong issuer", wrong_issuer),
            ("wrong client", wrong_client),
        ] {
            let result = check(&auth, &claims).await;
            assert!(
                matches!(result, Err(AuthError::Unauthorized(_))),
                "{name}: {result:?}"
            );
        }

        for header in [
            None,
            Some("Basic abc"),
            Some("Bearer "),
            Some("Bearer not-a-jwt"),
        ] {
            assert!(matches!(
                auth.authenticate(header).await,
                Err(AuthError::Unauthorized(_))
            ));
        }
    }

    #[tokio::test]
    async fn test_missing_group_is_forbidden() {
        let (_keycloak, auth, issuer) = setup().await;
        let mut claims = claims(&issuer);
        claims["groups"] = serde_json::json!(["users"]);
        assert_eq!(check(&auth, &claims).await, Err(AuthError::Forbidden));
        claims.as_object_mut().unwrap().remove("groups");
        assert_eq!(check(&auth, &claims).await, Err(AuthError::Forbidden));
    }

    #[tokio::test]
    async fn test_unknown_kid_refreshes_keys_once_and_is_rejected() {
        let (keycloak, auth, issuer) = setup().await;
        check(&auth, &claims(&issuer)).await.unwrap();
        let unknown = token_with_kid(&claims(&issuer), "rotated-key");
        let result = auth.authenticate(Some(&format!("Bearer {unknown}"))).await;
        assert!(
            matches!(result, Err(AuthError::Unauthorized(_))),
            "{result:?}"
        );
        // The keys were fetched less than MIN_REFRESH_INTERVAL ago, so no new request.
        assert_eq!(keycloak.received_requests().await.unwrap().len(), 1);

        let (keycloak, auth, issuer) = setup().await;
        let unknown = token_with_kid(&claims(&issuer), "rotated-key");
        assert!(
            auth.authenticate(Some(&format!("Bearer {unknown}")))
                .await
                .is_err()
        );
        assert_eq!(keycloak.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_token_signed_with_a_newly_published_key_is_accepted() {
        let keycloak = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks()))
            .up_to_n_times(1)
            .mount(&keycloak)
            .await;
        // After the rotation Keycloak publishes the new key next to the old one.
        let mut rotated = jwks();
        let mut new_key = rotated["keys"][1].clone();
        new_key["kid"] = "rotated-key".into();
        rotated["keys"].as_array_mut().unwrap().push(new_key);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rotated))
            .mount(&keycloak)
            .await;
        let issuer = format!("{}/realms/bakta", keycloak.uri());
        let auth = AdminAuth::new(AdminAuthConfig {
            issuer: issuer.clone(),
            client_id: TEST_CLIENT.to_string(),
            group: TEST_GROUP.to_string(),
        })
        .with_min_refresh_interval(Duration::ZERO);
        check(&auth, &claims(&issuer)).await.unwrap();

        let rotated_token = token_with_kid(&claims(&issuer), "rotated-key");
        let user = auth
            .authenticate(Some(&format!("Bearer {rotated_token}")))
            .await
            .unwrap();
        assert_eq!(user.username, "jdoe");
        assert_eq!(keycloak.received_requests().await.unwrap().len(), 2);
        // The old key still works.
        check(&auth, &claims(&issuer)).await.unwrap();
    }

    #[tokio::test]
    async fn test_failed_refreshes_are_throttled() {
        let keycloak = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&keycloak)
            .await;
        let issuer = format!("{}/realms/bakta", keycloak.uri());
        let auth = AdminAuth::new(AdminAuthConfig {
            issuer: issuer.clone(),
            client_id: TEST_CLIENT.to_string(),
            group: TEST_GROUP.to_string(),
        });
        for _ in 0..3 {
            let result = check(&auth, &claims(&issuer)).await;
            assert!(
                matches!(result, Err(AuthError::Unavailable(_))),
                "{result:?}"
            );
        }
        assert_eq!(keycloak.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_validation_does_not_wait_for_a_running_refresh() {
        let keycloak = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks()))
            .up_to_n_times(1)
            .mount(&keycloak)
            .await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(jwks())
                    .set_delay(Duration::from_secs(3)),
            )
            .mount(&keycloak)
            .await;
        let issuer = format!("{}/realms/bakta", keycloak.uri());
        let auth = std::sync::Arc::new(
            AdminAuth::new(AdminAuthConfig {
                issuer: issuer.clone(),
                client_id: TEST_CLIENT.to_string(),
                group: TEST_GROUP.to_string(),
            })
            .with_min_refresh_interval(Duration::ZERO),
        );
        check(&auth, &claims(&issuer)).await.unwrap();

        // An unknown key starts a slow refresh.
        let slow = {
            let auth = auth.clone();
            let token = token_with_kid(&claims(&issuer), "rotated-key");
            tokio::spawn(async move { auth.authenticate(Some(&format!("Bearer {token}"))).await })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;

        let other = token_with_kid(&claims(&issuer), "other-key");
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            auth.authenticate(Some(&format!("Bearer {other}"))),
        )
        .await
        .expect("validation must not wait for the refresh");
        assert!(
            matches!(result, Err(AuthError::Unavailable(_))),
            "{result:?}"
        );
        // Known keys keep working during the refresh.
        tokio::time::timeout(Duration::from_secs(1), check(&auth, &claims(&issuer)))
            .await
            .expect("validation must not wait for the refresh")
            .unwrap();
        slow.abort();
    }

    #[tokio::test]
    async fn test_unreachable_keycloak_is_unavailable() {
        let auth = AdminAuth::new(AdminAuthConfig {
            issuer: "http://127.0.0.1:1/realms/bakta".to_string(),
            client_id: TEST_CLIENT.to_string(),
            group: TEST_GROUP.to_string(),
        });
        let result = check(&auth, &claims("http://127.0.0.1:1/realms/bakta")).await;
        assert!(
            matches!(result, Err(AuthError::Unavailable(_))),
            "{result:?}"
        );
    }
}
