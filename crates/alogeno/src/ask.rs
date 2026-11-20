//! The reverse proxy's question, before it obtains a certificate for a name
//! (Caddy's on-demand TLS): is this name served here? It is answered on a
//! second listener, on the loopback only (`domains.ask_listen`), from the
//! same table of addresses the HTTP dispatch uses, so that the proxy gets
//! certificates exactly for the names alogeno serves.

use std::collections::HashMap;

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};

use crate::resolver;

pub fn router(resolver: resolver::Shared) -> Router {
    Router::new().route("/ask", get(ask)).with_state(resolver)
}

/// `GET /ask?domain=<name>`: 200 when the name is served, 403 when not, 400
/// without a name.
async fn ask(
    State(resolver): State<resolver::Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> StatusCode {
    let Some(domain) = query.get("domain").filter(|domain| !domain.is_empty()) else {
        return StatusCode::BAD_REQUEST;
    };
    let allowed = resolver::current(&resolver).resolve(domain).is_some();
    eprintln!(
        "alogeno: ask {domain} {}",
        if allowed { "allow" } else { "deny" }
    );
    if allowed {
        StatusCode::OK
    } else {
        StatusCode::FORBIDDEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::domains::Domain;
    use crate::sites::Site;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn status(app: &Router, uri: &str) -> StatusCode {
        app.clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_proxy_gets_certificates_for_the_served_names_only() {
        let config = Config::parse(
            "listen = \"127.0.0.1:4100\"\ndata_dir = \"/d\"\n[hosts]\npanel = \"admin.test\"\n",
        )
        .unwrap();
        let sites = [Site {
            id: "A".into(),
            slug: "a".into(),
            name: "A".into(),
        }];
        let domains = [
            Domain {
                host: "a.test".into(),
                site_id: "A".into(),
                redirect_to: None,
            },
            Domain {
                host: "www.a.test".into(),
                site_id: "A".into(),
                redirect_to: Some("a.test".into()),
            },
        ];
        let app = router(resolver::Resolver::shared(&config, &sites, &domains));
        for allowed in ["admin.test", "a.preview.admin.test", "a.test", "www.a.test"] {
            assert_eq!(
                status(&app, &format!("/ask?domain={allowed}")).await,
                StatusCode::OK,
                "{allowed}"
            );
        }
        for denied in ["x.preview.admin.test", "evil.test", "preview.admin.test"] {
            assert_eq!(
                status(&app, &format!("/ask?domain={denied}")).await,
                StatusCode::FORBIDDEN,
                "{denied}"
            );
        }
        assert_eq!(status(&app, "/ask").await, StatusCode::BAD_REQUEST);
        assert_eq!(status(&app, "/ask?domain=").await, StatusCode::BAD_REQUEST);
        assert_eq!(status(&app, "/other").await, StatusCode::NOT_FOUND);
    }
}
