// SPDX-License-Identifier: MPL-2.0
//! HTTP transport and server bootstrapping for the companion server.

pub mod http_server;

pub use http_server::{
    ApiRoute, COMPANION_API_ROUTES, INSPECTION_GET_ROUTES, ServerConfig, app_for_paths,
    app_with_query, app_with_services, run_server,
};
