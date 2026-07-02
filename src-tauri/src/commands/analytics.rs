//! OpenFlow analytics commands (M4). Thin wrappers over [`AnalyticsManager`]
//! that the usage dashboard calls, plus the privacy-mode setter and a
//! clear-all-data action.

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::managers::analytics::{
    AnalyticsManager, AnalyticsSummary, AppUsage, KeywordCount, OverTimePoint, ProjectUsage,
};
use crate::settings::{self, AnalyticsPrivacy};

#[tauri::command]
#[specta::specta]
pub async fn get_analytics_summary(
    analytics: State<'_, Arc<AnalyticsManager>>,
    range_days: Option<i64>,
) -> Result<AnalyticsSummary, String> {
    analytics.summary(range_days).map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_dictations_over_time(
    analytics: State<'_, Arc<AnalyticsManager>>,
    range_days: Option<i64>,
) -> Result<Vec<OverTimePoint>, String> {
    analytics.over_time(range_days).map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_analytics_by_app(
    analytics: State<'_, Arc<AnalyticsManager>>,
    range_days: Option<i64>,
) -> Result<Vec<AppUsage>, String> {
    analytics.by_app(range_days).map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_analytics_by_project(
    analytics: State<'_, Arc<AnalyticsManager>>,
    range_days: Option<i64>,
) -> Result<Vec<ProjectUsage>, String> {
    analytics.by_project(range_days).map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_top_keywords(
    analytics: State<'_, Arc<AnalyticsManager>>,
    range_days: Option<i64>,
    limit: Option<i64>,
) -> Result<Vec<KeywordCount>, String> {
    analytics
        .top_keywords(range_days, limit)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn set_analytics_privacy(app: AppHandle, mode: AnalyticsPrivacy) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    s.analytics_privacy = mode;
    settings::write_settings(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn clear_analytics(analytics: State<'_, Arc<AnalyticsManager>>) -> Result<(), String> {
    analytics.clear().map_err(|e| e.to_string())
}
