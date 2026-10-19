// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Distrib
// SPDX-License-Identifier: Apache-2.0

//! Hotel8:Distrib Admin UI handlers - RFW definitions.
//! Served from `GET /v1/admin/ui/{page}`, rendered by Sentin8:Dash at runtime.

use crate::handlers::AppState;
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use sentin8::rfw::page::*;
use sentin8::rfw::response::AdminUiResponse;
use sentin8::rfw::widget::*;
#[utoipa::path(
    get,
    path = "/v1/admin/ui/{page}",
    params(("page" = String, Path, description = "Admin UI page id (overview | channels | sync | policy pages)")),
    responses(
        (status = 200, description = "SDK AdminUiResponse envelope: { page: AdminPage (title + widgets: WidgetCard/WidgetDataTable/WidgetText with data_endpoint routes), status: \"ok\" } - see sentin8::rfw::page::AdminPage / sentin8::rfw::widget::RemoteWidget; policy pages reuse the shared Shield policy-studio definitions under the hotel8:distrib domain", body = sentin8::rfw::AdminUiResponse),
        (status = 404, description = "Unknown admin UI page"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib-admin",
)]

pub async fn get_admin_ui(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(page): Path<String>,
) -> Result<Json<AdminUiResponse>, (axum::http::StatusCode, String)> {
    let _a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &_a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let def = match page.as_str() {
        "overview" => AdminPage::new("Distribution Overview")
            .widget(card("Summary", "Channels, availability, settings."))
            .build(),
        "channels" => AdminPage::new("Channels")
            .widget(table(
                vec![
                    col("id", "Channel ID"),
                    cols("name", "Channel"),
                    cols("status", "Status"),
                ],
                "/channels",
            ))
            .build(),
        "availability" => AdminPage::new("Availability")
            .widget(card("Availability", "Room availability by date, channel."))
            .build(),
        "settings" => AdminPage::new("Distribution Settings")
            .widget(card("Settings", "Hotel8:Distrib configuration."))
            .build(),
        // ── Policy Studio - shared page
        // constructors; the guard guarantees `Some` (§3.7 dispatch).
        p if sentin8::rfw::policy_page("hotel8:distrib", p).is_some() => {
            sentin8::rfw::policy_page("hotel8:distrib", p)
                .expect("policy_page checked in guard")
                .build()
        }
        _ => {
            return Err((
                axum::http::StatusCode::NOT_FOUND,
                format!("Unknown admin UI page: {page}"),
            ))
        }
    };
    Ok(Json(AdminUiResponse::page(def)))
}

#[allow(dead_code)]
fn col(key: &str, label: &str) -> WidgetDataTableColumn {
    WidgetDataTableColumn {
        key: key.into(),
        label: label.into(),
        sortable: false,
        width: None,
        format: None,
    }
}
#[allow(dead_code)]
fn cols(key: &str, label: &str) -> WidgetDataTableColumn {
    WidgetDataTableColumn {
        key: key.into(),
        label: label.into(),
        sortable: true,
        width: None,
        format: None,
    }
}
#[allow(dead_code)]
fn table(columns: Vec<WidgetDataTableColumn>, endpoint: &str) -> RemoteWidget {
    RemoteWidget::DataTable(WidgetDataTable {
        columns,
        data_endpoint: endpoint.into(),
        sortable: true,
        filterable: true,
        page_size: 20,
        empty_message: Some("No data.".into()),
        row_link: None,
        row_actions: vec![],
    })
}
fn card(title: &str, body: &str) -> RemoteWidget {
    RemoteWidget::Card(WidgetCard {
        title: Some(title.into()),
        subtitle: None,
        children: vec![RemoteWidget::Text(WidgetText {
            text: body.into(),
            text_type: WidgetTextType::Body,
            align: TextAlign::Left,
            max_lines: None,
        })],
        actions: vec![],
    })
}
#[allow(dead_code)]
fn link(label: &str, route: &str) -> AdminUiAction {
    AdminUiAction {
        label: label.into(),
        action_type: AdminUiActionType::Link,
        route: Some(route.into()),
        endpoint: None,
        icon: Some("0xe145".into()),
    }
}
