//! Code-first OpenAPI 3.1 for the authenticated route catalog.
use super::{Fault, Route, Target, schema};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use utoipa::{
    OpenApi, ToSchema,
    openapi::{
        Content, Header, HttpMethod, Object, PathsBuilder, Ref, Required, ResponseBuilder,
        SecurityRequirement, Server, Type,
        path::{OperationBuilder, Parameter, ParameterBuilder, ParameterIn, PathItemBuilder},
        request_body::RequestBodyBuilder,
        security::SecurityScheme,
    },
};

#[derive(Serialize, ToSchema)]
pub(super) struct ManagementError {
    pub error: &'static str,
    #[serde(rename = "_links")]
    #[schema(value_type=Object)]
    pub links: Value,
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Repeater controller management", version = "1"),
    components(schemas(ManagementError))
)]
struct ManagementApi;

fn parameter(
    name: &str,
    location: ParameterIn,
    required: bool,
    description: &str,
    ty: Type,
) -> Parameter {
    ParameterBuilder::new()
        .name(name)
        .parameter_in(location)
        .description(Some(description))
        .required(if required {
            Required::True
        } else {
            Required::False
        })
        .schema(Some(Object::with_type(ty)))
        .build()
}

pub(super) fn media(route: &Route) -> &'static str {
    match route.target {
        Target::OpenApi => "application/json",
        Target::Schema => "application/schema+json",
        _ => "application/hal+json",
    }
}

fn method(name: &str) -> HttpMethod {
    match name {
        "GET" => HttpMethod::Get,
        "PUT" => HttpMethod::Put,
        "POST" => HttpMethod::Post,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        _ => unreachable!("registered method"),
    }
}

pub(super) fn document(mount: &str, routes: &BTreeMap<String, Route>) -> Value {
    let mut api = ManagementApi::openapi();
    api.servers = Some(vec![Server::new(mount)]);
    api.security = Some(vec![SecurityRequirement::new(
        "service",
        Vec::<String>::new(),
    )]);
    let components = api.components.as_mut().expect("documented error schema");
    components.add_security_scheme(
        "service",
        SecurityScheme::MutualTls {
            description: Some(
                "Every backend request requires an approved service client certificate.".into(),
            ),
            extensions: None,
        },
    );
    let errors = [
        Fault::Malformed,
        Fault::Forbidden,
        Fault::Missing,
        Fault::Method,
        Fault::Accept,
        Fault::Conflict,
        Fault::Gone,
        Fault::PreconditionFailed,
        Fault::TooLarge,
        Fault::Media,
        Fault::Invalid,
        Fault::PreconditionRequired,
        Fault::Unavailable,
    ];
    for error in errors {
        let (status, code) = error.wire();
        components.responses.insert(
            status.as_u16().to_string(),
            ResponseBuilder::new()
                .description(code)
                .content(
                    "application/hal+json",
                    Content::new(Some(Ref::from_schema_name("ManagementError"))),
                )
                .build()
                .into(),
        );
    }
    let mut paths = PathsBuilder::new();
    for route in routes.values() {
        let browser_signaling = route.target.is_webrtc();
        let mut item = PathItemBuilder::new();
        let parameters: Vec<_> = route
            .path
            .split('/')
            .filter_map(|v| v.strip_prefix('{')?.strip_suffix('}'))
            .map(|name| {
                let desc = if name == "name" {
                    "Public profile or relation name"
                } else {
                    "Canonical lowercase UUID"
                };
                parameter(name, ParameterIn::Path, true, desc, Type::String)
            })
            .collect();
        if !parameters.is_empty() {
            item = item.parameters(Some(parameters));
        }
        for &name in route.methods {
            let mut op = OperationBuilder::new().summary(Some(format!("{name} {}", route.profile)));
            if browser_signaling {
                op = op.description(Some(
                    "Requires an OIDC browser principal asserted by an authorized mutual TLS reverse proxy. Session operations bind the principal, port, service boot and session.",
                ));
            }
            let created_session = name == "POST" && route.target == Target::WebRtcSessions;
            let status_capture =
                matches!(route.target, Target::StatusSnapshot | Target::StatusStream);
            let success = match (name, route.target) {
                ("GET", Target::StatusStream) => "101",
                ("GET", _)
                | (
                    _,
                    Target::Operation
                    | Target::MaintenanceLease
                    | Target::WebRtcSession
                    | Target::WebRtcCandidates,
                ) => "200",
                ("POST", _) => "201",
                _ => "202",
            };
            let mut result = ResponseBuilder::new().description(match success {
                "201" if created_session => "Created a private WebRTC signaling session",
                "201" => "Created a registered member",
                "202" => "Accepted desired configuration",
                _ if browser_signaling => "Current private signaling representation",
                _ => "Current representation or operation receipt",
            });
            let read = if name == "GET"
                || browser_signaling
                || matches!(route.target, Target::Operation | Target::MaintenanceLease)
            {
                let response_route = if created_session {
                    routes
                        .values()
                        .find(|route| route.target == Target::WebRtcSession)
                        .expect("registered WebRTC session resource")
                } else {
                    route
                };
                Content::new(Some(Ref::new(format!(
                    "{mount}/schemas/{}/",
                    schema::response_name(response_route)
                ))))
            } else {
                Content::new(Some(Object::with_type(Type::Object)))
            };
            if route.target == Target::StatusStream {
                result = result
                    .description("RFC 6455 read-only status. Negotiate rptadv.status.v1 over the authenticated loopback HTTPS listener.")
                    .header("Sec-WebSocket-Protocol", Header::new(Object::with_type(Type::String)));
                op = op.response(
                    "426",
                    ResponseBuilder::new()
                        .description(
                            "Upgrade required. The HAL response links to the scoped snapshot.",
                        )
                        .content(
                            "application/hal+json",
                            Content::new(Some(Ref::from_schema_name("ManagementError"))),
                        )
                        .build(),
                );
                op = op.parameter(parameter(
                    "meter_hz",
                    ParameterIn::Query,
                    false,
                    "Delivery rate for meters. 1, 2, 5, 10 or 20. Default is 20.",
                    Type::Integer,
                ));
            } else {
                result = result.content(media(route), read);
            }
            if status_capture {
                for (name, desc, ty) in [
                    (
                        "site",
                        "Use true to include the site. The site is the default when no scope is supplied.",
                        Type::Boolean,
                    ),
                    (
                        "port",
                        "Repeat once per selected canonical resource UUID.",
                        Type::String,
                    ),
                    (
                        "all_ports",
                        "Use true to include the site and all visible ports. Cannot be combined with port.",
                        Type::Boolean,
                    ),
                    (
                        "topics",
                        "Comma-separated topic names. Default includes applicable state. meters is opt-in.",
                        Type::String,
                    ),
                ] {
                    op = op.parameter(parameter(name, ParameterIn::Query, false, desc, ty));
                }
            }
            if route.target == Target::StatusSnapshot {
                for (name, desc, ty) in [
                    (
                        "cursor",
                        "Opaque snapshot continuation from the next HAL link.",
                        Type::String,
                    ),
                    (
                        "page",
                        "Continuation page from the next HAL link.",
                        Type::Integer,
                    ),
                ] {
                    op = op.parameter(parameter(name, ParameterIn::Query, false, desc, ty));
                }
                op = op.response(
                    "410",
                    ResponseBuilder::new()
                        .description("Snapshot expired. The HAL response links to a fresh capture.")
                        .content(
                            "application/hal+json",
                            Content::new(Some(Ref::from_schema_name("ManagementError"))),
                        )
                        .build(),
                );
            }
            if name == "GET" && !status_capture && !browser_signaling {
                result = result.header("ETag", Header::new(Object::with_type(Type::String)));
            }
            if name == "POST" && route.target != Target::WebRtcCandidates {
                result = result.header("Location", Header::new(Object::with_type(Type::String)));
            }
            let result = result.build();
            if route.target == Target::MaintenanceLease && name == "PUT" {
                op = op.response("201", result.clone());
            }
            op = op.response(success, result);
            for error in errors {
                let status = error.wire().0.as_u16().to_string();
                if browser_signaling
                    && matches!(
                        error,
                        Fault::PreconditionFailed | Fault::PreconditionRequired
                    )
                {
                    continue;
                }
                if name == "GET"
                    && !(browser_signaling && error == Fault::Gone)
                    && !matches!(
                        error,
                        Fault::Malformed
                            | Fault::Forbidden
                            | Fault::Missing
                            | Fault::Method
                            | Fault::Accept
                            | Fault::Unavailable
                    )
                {
                    continue;
                }
                op = op.response(&status, Ref::from_response_name(&status));
            }
            if name == "GET"
                && matches!(
                    route.target,
                    Target::Collection(_) | Target::Peers | Target::Calls
                )
            {
                for q in ["offset", "limit"] {
                    op = op.parameter(parameter(
                        q,
                        ParameterIn::Query,
                        false,
                        "Window of embedded collection members",
                        Type::Integer,
                    ));
                }
            }
            if route.target == Target::WebRtcSession && name == "DELETE" {
                op = op.parameter(parameter(
                    "ice_generation",
                    ParameterIn::Query,
                    true,
                    "Canonical decimal generation from the close link of the current session.",
                    Type::String,
                ));
            }
            if name != "GET" && !browser_signaling {
                let create = name == "PUT"
                    && matches!(route.target, Target::Operation | Target::MaintenanceLease);
                op = op.parameter(parameter(
                    "If-Match",
                    ParameterIn::Header,
                    !create,
                    "Current strong validator for the exact canonical target",
                    Type::String,
                ));
                if create {
                    op = op.parameter(parameter(
                        "If-None-Match",
                        ParameterIn::Header,
                        false,
                        "Use * for first creation instead of If-Match",
                        Type::String,
                    ));
                }
            }
            if matches!(name, "POST" | "PUT" | "PATCH") {
                let (content, profile) = if name == "PATCH" {
                    ("application/json-patch+json", "json-patch".to_owned())
                } else {
                    ("application/hal+json", schema::name(route, name == "POST"))
                };
                op = op.request_body(Some(
                    RequestBodyBuilder::new()
                        .required(Some(Required::True))
                        .content(
                            content,
                            Content::new(Some(Ref::new(format!("{mount}/schemas/{profile}/")))),
                        )
                        .build(),
                ));
            }
            item = item.operation(method(name), op);
        }
        paths = paths.path(&route.path, item.build());
    }
    api.paths = paths.build();
    let mut document = serde_json::to_value(api).expect("serializable OpenAPI document");
    document["x-rptadv-status-stream"] = super::status::profile(true);
    document
}

#[cfg(test)]
mod tests {
    use super::super::{Api, Observed};
    use super::*;

    #[test]
    fn webrtc_openapi_uses_browser_runtime_schemas_and_generation_preconditions() {
        let api = Api::with_webrtc("/api/v1", true).unwrap();
        let description = document(&api.mount, &api.routes);
        let paths = &description["paths"];
        let collection = &paths["/webrtc/ports/{port}/sessions/"];
        let member = &paths["/webrtc/ports/{port}/sessions/{boot}/{session}/"];
        let candidates = &paths["/webrtc/ports/{port}/sessions/{boot}/{session}/candidates/"];

        assert_eq!(
            collection["post"]["responses"]["201"]["content"]["application/hal+json"]["schema"]["$ref"],
            "/api/v1/schemas/webrtc-session-response/"
        );
        assert_eq!(
            candidates["post"]["requestBody"]["content"]["application/hal+json"]["schema"]["$ref"],
            "/api/v1/schemas/webrtc-candidates-create/"
        );
        assert!(
            collection["post"]["description"]
                .as_str()
                .unwrap()
                .contains("OIDC browser principal")
        );
        assert!(member.get("patch").is_none());
        assert!(member["put"].get("parameters").is_none());
        assert_eq!(member["delete"]["parameters"][0]["name"], "ice_generation");
        assert_eq!(member["delete"]["parameters"][0]["required"], true);
        assert!(candidates["post"]["responses"]["200"].is_object());

        let live = Observed::default();
        let profile = "/profiles/webrtc-sessions/";
        let (route, params) = api
            .lookup(&format!("{}{profile}", api.mount))
            .map(|(r, _)| (r, r.match_path(profile).unwrap()))
            .unwrap();
        let profile = api.read(route, params, profile, None, &live, None).unwrap();
        assert_eq!(
            profile["_links"]["describedby"]["href"],
            "/api/v1/schemas/webrtc-sessions-response/"
        );
        assert_eq!(
            profile["_links"]["rpt:request"]["href"],
            "/api/v1/schemas/webrtc-sessions-create/"
        );
    }
}
