use crate::structs::User;
use actix_web::{
    Error, FromRequest, HttpRequest, HttpResponse, dev::Payload, http::header::LOCATION,
};
use argon2::Config;
use futures::future::{Ready, ready};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::{RngCore, rng};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub fn hash_password(password: &str) -> Result<String, argon2::Error> {
    let mut salt = vec![0u8; 16];
    rng().fill_bytes(&mut salt);

    let config = Config::default();
    let hash = argon2::hash_encoded(password.as_bytes(), &salt, &config)?;

    Ok(hash)
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match argon2::verify_encoded(hash, password.as_bytes()) {
        Ok(matches) => matches,
        Err(_) => false,
    }
}

#[derive(Debug, Error)]
pub enum JwtError {
    #[error("JWT_SECRET not set")]
    SecretNotSet,
    #[error("Error calculating expiration time: {0}")]
    ExpirationError(#[from] std::time::SystemTimeError),
    #[error("Error encoding the JWT")]
    JwtEncodingError,
    #[error("Error decoding the JWT")]
    JwtDecodingError,
    #[error("JWT has expired")]
    JwtExpired,
    #[error("Token not found in request")]
    TokenNotFound,
    #[error("Unauthorized access")]
    Unauthorized,
}

#[derive(Debug)]
pub enum AuthError {
    Redirect(HttpResponse),
    Other(Error),
}

impl From<JwtError> for AuthError {
    fn from(err: JwtError) -> Self {
        match err {
            JwtError::TokenNotFound
            | JwtError::JwtExpired
            | JwtError::JwtDecodingError
            | JwtError::Unauthorized => AuthError::Redirect(
                HttpResponse::Found()
                    .append_header((LOCATION, "/login"))
                    .finish(),
            ),
            _ => AuthError::Other(actix_web::error::ErrorInternalServerError(err.to_string())),
        }
    }
}

impl From<AuthError> for Error {
    fn from(err: AuthError) -> Error {
        match err {
            AuthError::Redirect(response) => {
                actix_web::error::InternalError::from_response("Authentication required", response)
                    .into()
            }
            AuthError::Other(err) => err,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct Claims {
    pub sub: i64,
    pub role: crate::structs::UserRole,
    pub exp: usize,
}

pub fn create_jwt(user: User, secret: &str) -> Result<String, JwtError> {
    let expiration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(JwtError::ExpirationError)?
        .as_secs()
        + 3600 * 1;

    let claims = Claims {
        sub: user.id,
        role: user.role,
        exp: expiration as usize,
    };

    let header = Header::default();
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());

    encode(&header, &claims, &encoding_key).map_err(|_| JwtError::JwtEncodingError)
}

pub fn read_jwt(req: &HttpRequest) -> Result<Claims, JwtError> {
    let token = req
        .cookie("token")
        .ok_or(JwtError::TokenNotFound)?
        .value()
        .to_string();

    let data = req
        .app_data::<actix_web::web::Data<crate::AppData>>()
        .ok_or(JwtError::SecretNotSet)?;
    let secret = &data.jwt_secret;

    let decoding_key = DecodingKey::from_secret(secret.as_bytes());
    let validation = Validation::new(jsonwebtoken::Algorithm::HS256);

    let token_data =
        decode::<Claims>(&token, &decoding_key, &validation).map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => JwtError::JwtExpired,
            _ => JwtError::JwtDecodingError,
        })?;

    Ok(token_data.claims)
}

#[derive(Debug)]
pub struct AuthUser {
    pub claims: Claims,
}

impl FromRequest for AuthUser {
    type Error = Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let result = read_jwt(req)
            .map(|claims| AuthUser { claims })
            .map_err(AuthError::from)
            .map_err(Error::from);

        ready(result)
    }
}

#[derive(Debug)]
pub struct AdminUser {
    pub claims: Claims,
}

impl FromRequest for AdminUser {
    type Error = Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, payload: &mut Payload) -> Self::Future {
        let auth_future = AuthUser::from_request(req, payload);

        let result = match auth_future.into_inner() {
            Ok(auth_user) => {
                if auth_user.claims.role == crate::structs::UserRole::Admin {
                    Ok(AdminUser {
                        claims: auth_user.claims,
                    })
                } else {
                    Err(AuthError::from(JwtError::Unauthorized).into())
                }
            }
            Err(e) => Err(e),
        };

        ready(result)
    }
}
