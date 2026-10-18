//! Native presentation of upstream LimitsSurface.tsx / limits.css (v6.1.10).
//! Copyright (c) 2026 codex-chatgpt-web contributors; MIT, assets/UPSTREAM-LICENSE.
use crate::ui::{Action, Launcher, icon};
use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px, relative, rgb, rgba};

// Pure mapping has no browser, account, filesystem or operation side effects.
mod mapping {
    use chrono::{Datelike, Timelike};
    use serde_json::Value;
    use std::sync::LazyLock;

    static COPY: LazyLock<Value> = LazyLock::new(|| {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/limits-copy.json"
        )))
        .expect("pinned limits copy")
    });
    static POLICY: LazyLock<Value> = LazyLock::new(|| {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/upstream/launcher/electron/limits-policy.json"
        )))
        .expect("pinned limits policy")
    });
    pub(super) const REFERENCE_URL: &str =
        "https://help.openai.com/en/articles/20001354-gpt-56-and-gpt-6-pro-in-chatgpt";
    #[derive(Clone, Copy)]
    pub(super) struct Copy<'a>(pub &'a str);
    impl Copy<'_> {
        pub fn text(self, key: &str) -> String {
            COPY.get(self.0).unwrap_or(&COPY["en"])[key]
                .as_str()
                .expect("limits copy key")
                .into()
        }
        pub fn count(self, key: &str, count: f64) -> String {
            self.text(key).replace("{count}", &number(count))
        }
        pub fn timestamp(self, value: &Value) -> String {
            value
                .as_i64()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|date| format_date(self.0, date.with_timezone(&chrono::Local), true))
                .unwrap_or_else(|| self.text("unknown"))
        }
    }
    #[derive(Default)]
    pub(super) struct Tracker<'a> {
        pub reading: bool,
        pub setting_up: bool,
        pub read_error: Option<&'a str>,
        pub setup_error: Option<&'a str>,
    }
    pub(super) struct Page {
        pub has_snapshot: bool,
        pub loading: bool,
        pub tracking: bool,
        pub unavailable: bool,
        pub setup_enabled: bool,
        pub setup_primary: bool,
        pub refresh_enabled: bool,
        pub status: String,
        pub title: String,
        pub body: String,
        pub setup_label: String,
        pub checked: Option<String>,
        pub busy_message: Option<String>,
        pub errors: Vec<(String, Option<String>)>,
        pub windows: Vec<Usage>,
        pub attribution: Option<String>,
        pub history: Option<History>,
    }
    pub(super) struct History {
        pub total: String,
        pub unknown_pro: String,
        pub since: String,
        pub uncertainty: bool,
    }
    pub(super) struct Usage {
        pub title: String,
        pub duration: String,
        pub observed: String,
        pub lower_bound: bool,
        pub warning: bool,
        pub meter: Option<f32>,
        pub reference: String,
        pub uncertainty: Option<String>,
    }
    pub(super) struct Reference {
        pub title: String,
        pub rows: Vec<(String, String)>,
        pub notes: Vec<String>,
        pub unavailable: bool,
        pub reference_only: bool,
    }
    pub(super) fn page(
        snapshot: &Value,
        manual: bool,
        runtime_busy: bool,
        tracker: Tracker<'_>,
        copy: Copy<'_>,
    ) -> Page {
        let has_snapshot = snapshot.get("enabled").is_some_and(Value::is_boolean);
        let plan = snapshot["plan"].as_str();
        let zero_risk = manual || snapshot["disabledReason"] == "zero-risk";
        let unsupported = plan == Some("unsupported");
        let supported = matches!(plan, Some("pro_100" | "pro_200"));
        let tracking = snapshot["enabled"] == true && supported && !zero_risk;
        let status_key = if zero_risk {
            "zeroRisk"
        } else if unsupported {
            "proOnly"
        } else if tracking {
            "active"
        } else if supported {
            "paused"
        } else {
            "notConfigured"
        };
        let title = if zero_risk {
            copy.text("zeroRisk")
        } else if unsupported {
            copy.text("proOnly")
        } else if tracking {
            if plan == Some("pro_200") {
                "Pro 20x ($200)".into()
            } else {
                "Pro 5x ($100)".into()
            }
        } else {
            copy.text("setupTitle")
        };
        let body_key = if zero_risk {
            "zeroRiskBody"
        } else if unsupported {
            "unsupportedBody"
        } else if tracking {
            "observedBody"
        } else {
            "setupBody"
        };
        let checked = snapshot["checkedAt"].as_i64().map(|_| {
            format!(
                "{}: {}",
                copy.text("checked"),
                copy.timestamp(&snapshot["checkedAt"])
            )
        });
        let mut errors = Vec::new();
        if let Some(message) = tracker.read_error {
            errors.push((
                copy.text(if has_snapshot {
                    "staleError"
                } else {
                    "loadError"
                }),
                (!message.is_empty()).then(|| message.into()),
            ));
        }
        if let Some(error) = tracker
            .setup_error
            .filter(|s| !s.is_empty())
            .or_else(|| snapshot["error"].as_str().filter(|s| !s.is_empty()))
        {
            errors.push((error.into(), None));
        }
        let windows = if tracking {
            snapshot["windows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|window| usage(window, copy))
                .collect()
        } else {
            Vec::new()
        };
        let unknown = snapshot["unknownProMessages"].as_u64().unwrap_or(0);
        let history = snapshot["trackingSince"].as_i64().map(|_| History {
            total: snapshot["totalMessages"]
                .as_f64()
                .map(number)
                .unwrap_or_else(|| copy.text("unknown")),
            unknown_pro: snapshot["unknownProMessages"]
                .as_f64()
                .map(number)
                .unwrap_or_else(|| copy.text("unknown")),
            since: copy.timestamp(&snapshot["trackingSince"]),
            uncertainty: unknown > 0,
        });
        Page {
            has_snapshot,
            loading: !has_snapshot && tracker.reading,
            tracking,
            unavailable: zero_risk || unsupported,
            setup_enabled: has_snapshot && !zero_risk && !runtime_busy && !tracker.setting_up,
            setup_primary: !tracking && !zero_risk && !unsupported,
            refresh_enabled: !tracker.reading && !tracker.setting_up,
            status: copy.text(status_key),
            title,
            body: copy.text(body_key),
            setup_label: copy.text(if tracker.setting_up {
                "checking"
            } else if checked.is_some() {
                "recheck"
            } else {
                "setup"
            }),
            checked,
            busy_message: (runtime_busy && !zero_risk).then(|| copy.text("browserBusy")),
            errors,
            windows,
            attribution: (tracking && unknown > 0).then(|| copy.text("attributionBody")),
            history,
        }
    }
    fn usage(window: &Value, copy: Copy<'_>) -> Option<Usage> {
        let used = window["used"].as_f64()?;
        let limit = window["limit"].as_f64()?;
        let duration = window["durationMs"].as_f64()?;
        let uncertain = window["uncertainUsed"].as_f64()?;
        if [used, limit, duration, uncertain]
            .into_iter()
            .any(|n| !n.is_finite() || n < 0.)
        {
            return None;
        }
        let shared = window["model"] == "shared";
        let lower_bound = uncertain > 0. && !shared;
        Some(Usage {
            title: match window["model"].as_str() {
                Some("gpt-6-pro") => "GPT-6 Pro".into(),
                Some("gpt-5.6-pro") => "GPT-5.6 Sol Pro".into(),
                _ => copy.text("combined"),
            },
            duration: if duration == 86400000. {
                copy.text("rollingDay")
            } else if duration == 604800000. {
                copy.text("rollingWeek")
            } else {
                copy.text("rollingHours")
                    .replace("{hours}", &number(duration / 3600000.))
            },
            observed: if lower_bound {
                copy.count("lowerBound", used)
            } else {
                number(used)
            },
            lower_bound,
            warning: limit > 0. && used >= limit * 0.75,
            meter: (!lower_bound).then(|| {
                if limit > 0. {
                    (used / limit).clamp(0., 1.) as f32
                } else {
                    0.
                }
            }),
            reference: copy.count("referenceCap", limit),
            uncertainty: (uncertain > 0.).then(|| {
                copy.count(
                    if shared { "sharedUnknown" } else { "uncertain" },
                    uncertain,
                )
            }),
        })
    }
    pub(super) fn reference_plans(copy: Copy<'_>) -> Vec<Reference> {
        let p = &*POLICY;
        let count = |key: &str, value: &Value| {
            copy.count(key, value.as_f64().expect("pinned policy number"))
        };
        vec![
            Reference {
                title: "Pro 20x ($200)".into(),
                rows: vec![
                    (
                        "GPT-6 Pro".into(),
                        count("week", &p["pro_200"]["gpt6Weekly"]),
                    ),
                    (
                        "GPT-5.6 Sol Pro".into(),
                        count("day", &p["pro_200"]["solDaily"]),
                    ),
                    (
                        copy.text("combined"),
                        count("day", &p["pro_200"]["combinedDaily"]),
                    ),
                ],
                notes: vec![],
                unavailable: false,
                reference_only: false,
            },
            Reference {
                title: "Pro 5x ($100)".into(),
                rows: vec![(
                    copy.text("shared"),
                    count("week", &p["pro_100"]["combinedWeekly"]),
                )],
                notes: vec!["GPT-6 Pro + GPT-5.6 Sol Pro".into()],
                unavailable: false,
                reference_only: false,
            },
            Reference {
                title: "Business".into(),
                rows: vec![
                    (
                        "Standard".into(),
                        count("month", &p["business"]["standardMonthly"]),
                    ),
                    (
                        "Premium".into(),
                        count("week", &p["business"]["premiumWeekly"]),
                    ),
                ],
                notes: vec![copy.text("shared"), copy.text("businessBody")],
                unavailable: false,
                reference_only: true,
            },
            Reference {
                title: copy.text("otherPlans"),
                rows: vec![],
                notes: vec![copy.text("otherPlansBody")],
                unavailable: true,
                reference_only: false,
            },
        ]
    }
    pub(super) fn reference_date(copy: Copy<'_>) -> String {
        let date = chrono::NaiveDate::parse_from_str(
            POLICY["checkedOn"].as_str().expect("policy checkedOn"),
            "%Y-%m-%d",
        )
        .expect("policy date");
        copy.text("asOf").replace(
            "{date}",
            &format_date(copy.0, date.and_hms_opt(0, 0, 0).unwrap().and_utc(), false),
        )
    }
    fn number(value: f64) -> String {
        let rounded = if value.fract() == 0. {
            value
        } else {
            (value * 1000.).round() / 1000.
        };
        let raw = format!("{:.3}", rounded);
        let (integer, fraction) = raw.split_once('.').unwrap();
        let mut grouped = String::new();
        for (i, ch) in integer.chars().enumerate() {
            if i > 0 && (integer.len() - i) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(ch);
        }
        let fraction = fraction.trim_end_matches('0');
        if !fraction.is_empty() {
            grouped.push('.');
            grouped.push_str(fraction);
        }
        grouped
    }
    fn format_date<T: chrono::TimeZone>(
        language: &str,
        d: chrono::DateTime<T>,
        time: bool,
    ) -> String {
        let (y, m, day, h, minute) = (d.year(), d.month(), d.day(), d.hour(), d.minute());
        let date = match language {
            "zh-CN" | "zh-TW" => format!("{y}年{m}月{day}日"),
            "ja" => format!("{y}/{m:02}/{day:02}"),
            "ko" => format!("{y}. {m}. {day}."),
            _ => format!(
                "{} {day}, {y}",
                [
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                    "Dec"
                ][(m - 1) as usize]
            ),
        };
        if !time {
            return date;
        }
        let hour = if h % 12 == 0 { 12 } else { h % 12 };
        match language {
            "zh-CN" | "ja" => format!("{date} {h:02}:{minute:02}"),
            "zh-TW" => format!(
                "{date} {}{hour}:{minute:02}",
                if h < 12 { "上午" } else { "下午" }
            ),
            "ko" => format!(
                "{date} {} {hour}:{minute:02}",
                if h < 12 { "오전" } else { "오후" }
            ),
            _ => format!(
                "{date}, {hour}:{minute:02} {}",
                if h < 12 { "AM" } else { "PM" }
            ),
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        fn snapshot() -> Value {
            json!({"enabled":true,"plan":"pro_200","trackingSince":0,"checkedAt":0,"totalMessages":301,"unknownProMessages":1,"windows":[{"model":"gpt-6-pro","used":250,"limit":200,"uncertainUsed":1,"durationMs":604800000},{"model":"shared","used":301,"limit":200,"uncertainUsed":1,"durationMs":86400000}]})
        }
        #[test]
        fn only_observed_counts_no_remaining_or_reset_estimate() {
            let mut value = snapshot();
            value["remaining"] = json!(999);
            value["resetAt"] = json!(123);
            let p = page(&value, false, false, Tracker::default(), Copy("en"));
            assert_eq!(p.windows[0].observed, "At least 250");
            assert_eq!(p.windows[0].meter, None);
            assert_eq!(p.windows[1].observed, "301");
            assert_eq!(p.windows[1].meter, Some(1.));
            assert!(p.windows[0].warning);
            assert!(p.windows[0].lower_bound);
            assert!(p.windows[0].uncertainty.as_ref().unwrap().contains("1"));
            assert!(
                p.windows[1]
                    .uncertainty
                    .as_ref()
                    .unwrap()
                    .starts_with("Includes 1")
            );
            assert_eq!(p.history.unwrap().total, "301");
            assert!(p.attribution.is_some());
        }
        #[test]
        fn manual_overrides_enabled_snapshot_but_keeps_history_and_public_plans() {
            let p = page(&snapshot(), true, true, Tracker::default(), Copy("en"));
            assert!(!p.tracking);
            assert!(!p.setup_enabled);
            assert!(p.windows.is_empty());
            assert!(p.history.is_some());
            assert!(p.busy_message.is_none());
            assert_eq!(p.status, "Unavailable in Zero Risk");
            assert_eq!(reference_plans(Copy("en")).len(), 4);
            let mut value = snapshot();
            value["disabledReason"] = json!("zero-risk");
            assert!(!page(&value, false, false, Tracker::default(), Copy("en")).tracking);
        }
        #[test]
        fn unsupported_unknown_and_paused_never_infer_account_capabilities() {
            for plan in [
                json!("unsupported"),
                json!(null),
                json!("business"),
                json!("unknown"),
            ] {
                let p = page(
                    &json!({"enabled":true,"plan":plan}),
                    false,
                    false,
                    Tracker::default(),
                    Copy("en"),
                );
                assert!(!p.tracking);
                assert!(p.windows.is_empty());
                assert!(p.history.is_none());
                assert!(p.setup_enabled);
            }
            let p = page(
                &json!({"enabled":false,"plan":"pro_100"}),
                false,
                false,
                Tracker::default(),
                Copy("en"),
            );
            assert_eq!(p.status, "Tracking not active");
            assert!(!p.tracking);
        }
        #[test]
        fn tracker_loading_stale_error_and_setup_error_are_distinct() {
            let p = page(
                &Value::Null,
                false,
                false,
                Tracker {
                    reading: true,
                    read_error: Some("offline"),
                    ..Default::default()
                },
                Copy("en"),
            );
            assert!(p.loading);
            assert!(!p.setup_enabled);
            assert!(!p.refresh_enabled);
            assert_eq!(p.errors[0].0, "Could not load tracking.");
            let loading = page(
                &json!({"reading":true}),
                false,
                false,
                Tracker {
                    reading: true,
                    ..Default::default()
                },
                Copy("en"),
            );
            assert!(loading.loading);
            assert!(!loading.has_snapshot);
            assert!(!loading.setup_enabled);

            let p = page(
                &snapshot(),
                false,
                true,
                Tracker {
                    setting_up: true,
                    read_error: Some(""),
                    setup_error: Some("setup failed"),
                    ..Default::default()
                },
                Copy("en"),
            );
            assert_eq!(p.errors[0].0, Copy("en").text("staleError"));
            assert_eq!(p.errors[0].1, None);
            assert_eq!(p.errors[1].0, "setup failed");
            assert_eq!(p.setup_label, "Checking your plan…");
            assert!(!p.refresh_enabled);
            assert!(!p.setup_enabled);
        }
        #[test]
        fn all_five_languages_keep_reference_policy_and_unknown_other_plans() {
            for language in ["en", "zh-CN", "zh-TW", "ja", "ko"] {
                let copy = Copy(language);
                let plans = reference_plans(copy);
                assert_eq!(plans[0].rows[0].1, copy.count("week", 200.));
                assert_eq!(plans[0].rows[1].1, copy.count("day", 170.));
                assert_eq!(plans[1].rows[0].1, copy.count("week", 50.));
                assert_eq!(plans[2].rows[0].1, copy.count("month", 15.));
                assert_eq!(plans[2].rows[1].1, copy.count("week", 50.));
                assert!(plans[3].rows.is_empty());
                assert!(plans[3].unavailable);
                assert!(plans[2].reference_only);
                assert_eq!(reference_date(copy).contains("2026"), true);
                assert_eq!(REFERENCE_URL, POLICY["sourceUrl"].as_str().unwrap());
            }
            assert_eq!(reference_date(Copy("en")), "As of Sep 19, 2026");
            assert_eq!(number(1234.5), "1,234.5");
            assert_eq!(number(1.2345), "1.235");
            let date = chrono::DateTime::parse_from_rfc3339("2026-09-19T13:04:00Z").unwrap();
            for (language, expected) in [
                ("en", "Sep 19, 2026, 1:04 PM"),
                ("zh-CN", "2026年9月19日 13:04"),
                ("zh-TW", "2026年9月19日 下午1:04"),
                ("ja", "2026/09/19 13:04"),
                ("ko", "2026. 9. 19. 오후 1:04"),
            ] {
                assert_eq!(format_date(language, date, true), expected);
            }
        }
        #[test]
        fn uncertainty_and_threshold_follow_upstream_without_adding_unknown_twice() {
            let mut value = snapshot();
            value["windows"][0]["used"] = json!(149);
            let p = page(&value, false, false, Tracker::default(), Copy("en"));
            assert!(!p.windows[0].warning);
            value["windows"][0]["used"] = json!(150);
            let p = page(&value, false, false, Tracker::default(), Copy("en"));
            assert!(p.windows[0].warning);
            assert_eq!(p.windows[1].observed, "301");
            value["windows"][0]["uncertainUsed"] = json!(0);
            value["windows"][0]["limit"] = json!(0);
            let p = page(&value, false, false, Tracker::default(), Copy("en"));
            assert!(!p.windows[0].warning);
            assert_eq!(p.windows[0].meter, Some(0.));
        }
    }
}

use mapping::{Copy, Page, Tracker};
const SECONDARY: u32 = 0xffffffb3;
const DIM: u32 = 0xffffff80;
const BORDER: u32 = 0xffffff14;

pub(crate) fn render_limits_surface(this: &Launcher, cx: &mut Context<Launcher>) -> AnyElement {
    let copy = Copy(&this.language);
    // Optional transient tracker fields belong to UI state, never the receipt store.
    let setting_up = this.limits["settingUp"].as_bool().unwrap_or(false)
        || this.operation.as_deref() == Some("Checking ChatGPT plan…");
    let page = mapping::page(
        &this.limits,
        this.manual(),
        this.busy(),
        Tracker {
            reading: this.limits["reading"].as_bool().unwrap_or(false),
            setting_up,
            read_error: this.limits["readError"].as_str(),
            setup_error: this.limits["setupError"].as_str(),
        },
        copy,
    );
    let width = (this.window_size.0
        - if this.state.sidebar_open {
            this.state.sidebar_width as f32
        } else {
            0.
        }
        - 64.)
        .min(720.);
    let narrow = width <= 540.;
    let mut children = vec![
        div()
            .flex()
            .items_start()
            .justify_between()
            .gap(px(16.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(this.heading(copy.text("title"), Some(copy.text("subtitle")))),
            )
            .child(if narrow {
                this.icon_button(
                    "reload",
                    "limits-refresh",
                    page.refresh_enabled,
                    Action::RefreshLimits,
                    cx,
                )
            } else {
                this.button(
                    "limits-refresh",
                    copy.text("refresh"),
                    false,
                    page.refresh_enabled,
                    Action::RefreshLimits,
                    cx,
                )
            })
            .into_any_element(),
    ];
    if page.loading {
        children.push(
            div()
                .py(px(20.))
                .text_size(px(12.))
                .text_color(rgba(DIM))
                .child(copy.text("loading"))
                .into_any_element(),
        );
    }
    for (title, message) in &page.errors {
        children.push(
            div()
                .flex()
                .gap(px(10.))
                .p(px(12.))
                .mb(px(14.))
                .rounded(px(10.))
                .border_1()
                .border_color(rgba(BORDER))
                .bg(rgba(0xfa423e21))
                .text_color(rgb(0xff6764))
                .text_size(px(12.))
                .line_height(px(19.))
                .child(icon("alert", 16.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_weight(FontWeight::MEDIUM).child(title.clone()))
                        .children(message.as_ref().map(|m| div().mt(px(4.)).child(m.clone()))),
                )
                .into_any_element(),
        );
    }
    if page.has_snapshot {
        children.push(setup(this, &page, narrow, cx));
    }
    if page.tracking {
        let columns = ((width + 10.) / 190.).floor().clamp(1., 3.) as u16;
        let mut observed = div()
            .mt(px(28.))
            .child(section_heading(
                this,
                copy.text("observedTitle"),
                Some(copy.text("estimate")),
            ))
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .p(px(12.))
                    .mb(px(14.))
                    .border_1()
                    .border_color(rgba(BORDER))
                    .rounded(px(10.))
                    .text_size(px(12.))
                    .line_height(px(19.))
                    .child(icon("info", 16.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(rgba(SECONDARY))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(copy.text("scopeTitle")),
                            )
                            .child(
                                div()
                                    .mt(px(4.))
                                    .text_color(rgba(DIM))
                                    .child(copy.text("scopeBody")),
                            ),
                    ),
            )
            .child(
                div()
                    .grid()
                    .grid_cols(columns)
                    .gap(px(10.))
                    .children(page.windows.iter().map(|window| usage_card(window, copy))),
            );
        if let Some(text) = &page.attribution {
            observed = observed.child(this.notice(text.clone(), true));
        }
        if page.windows.is_empty() {
            observed = observed.child(this.notice(copy.text("noWindows"), false));
        }
        children.push(observed.into_any_element());
    }
    if let Some(history) = &page.history {
        let cell = |label: String, value: String, count: bool| {
            div()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(11.))
                        .line_height(px(17.))
                        .text_color(rgba(DIM))
                        .child(label),
                )
                .child(
                    div()
                        .mt(px(8.))
                        .text_size(px(if count { 20. } else { 12. }))
                        .text_color(rgba(SECONDARY))
                        .child(value),
                )
        };
        children.push(
            div()
                .mt(px(28.))
                .child(section_heading(this, copy.text("history"), None))
                .child(description(copy.text("historyBody")))
                .child(
                    div()
                        .grid()
                        .grid_cols(if narrow { 2 } else { 3 })
                        .gap(px(if narrow { 16. } else { 20. }))
                        .py(px(16.))
                        .border_t_1()
                        .border_b_1()
                        .border_color(rgba(BORDER))
                        .child(cell(
                            copy.text("totalMessages"),
                            history.total.clone(),
                            true,
                        ))
                        .child(cell(
                            copy.text("unknownPro"),
                            history.unknown_pro.clone(),
                            true,
                        ))
                        .child(
                            cell(copy.text("since"), history.since.clone(), false)
                                .when(narrow, |d| d.col_span_full()),
                        ),
                )
                .when(history.uncertainty, |d| {
                    d.child(
                        div()
                            .mt(px(9.))
                            .text_size(px(11.))
                            .line_height(px(18.))
                            .text_color(rgba(DIM))
                            .child(copy.text("unknownProBody")),
                    )
                })
                .into_any_element(),
        );
    }
    children.push(
        div()
            .mt(px(28.))
            .child(section_heading(
                this,
                copy.text("referenceTitle"),
                Some(copy.text("referenceOnly")),
            ))
            .child(description(copy.text("referenceBody")))
            .child(
                div()
                    .grid()
                    .grid_cols(if narrow { 1 } else { 2 })
                    .gap(px(10.))
                    .children(
                        mapping::reference_plans(copy)
                            .into_iter()
                            .map(|plan| reference_card(plan, copy)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .mt(px(10.))
                    .text_size(px(11.))
                    .text_color(rgba(DIM))
                    .child(mapping::reference_date(copy))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(this.button(
                                "limits-source",
                                copy.text("source"),
                                false,
                                true,
                                Action::External(mapping::REFERENCE_URL),
                                cx,
                            ))
                            .child(icon("external", 13.)),
                    ),
            )
            .into_any_element(),
    );
    this.content(children, false)
}

fn setup(this: &Launcher, page: &Page, narrow: bool, cx: &mut Context<Launcher>) -> AnyElement {
    let main = div()
        .min_w_0()
        .flex_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(7.))
                .mb(px(12.))
                .text_size(px(11.))
                .text_color(rgba(DIM))
                .child(div().size(px(6.)).rounded_full().bg(rgb(if page.tracking {
                    0x40c977
                } else {
                    0x808080
                })))
                .child(page.status.clone()),
        )
        .child(
            div()
                .text_size(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .child(page.title.clone()),
        )
        .child(
            div()
                .mt(px(7.))
                .text_size(px(12.))
                .line_height(px(19.))
                .text_color(rgba(if page.unavailable { DIM } else { SECONDARY }))
                .child(page.body.clone()),
        )
        .children(page.checked.as_ref().map(|text| {
            div()
                .mt(px(12.))
                .text_size(px(11.))
                .text_color(rgba(DIM))
                .child(text.clone())
        }));
    div()
        .p(px(if narrow { 16. } else { 20. }))
        .rounded(px(14.))
        .border_1()
        .border_color(rgba(BORDER))
        .bg(rgba(0xffffff08))
        .text_color(rgba(if page.unavailable { DIM } else { 0xffffffff }))
        .child(
            div()
                .flex()
                .gap(px(if narrow { 14. } else { 24. }))
                .when(narrow, |d| d.flex_col())
                .when(!narrow, |d| d.items_center())
                .child(main)
                .child(div().flex().flex_shrink_0().child(this.button(
                    "setup-limits",
                    page.setup_label.clone(),
                    page.setup_primary,
                    page.setup_enabled,
                    Action::Tracking,
                    cx,
                ))),
        )
        .children(page.busy_message.as_ref().map(|text| {
            div()
                .mt(px(16.))
                .pt(px(12.))
                .border_t_1()
                .border_color(rgba(BORDER))
                .text_size(px(11.))
                .text_color(rgba(DIM))
                .child(text.clone())
        }))
        .into_any_element()
}
fn section_heading(this: &Launcher, title: String, tag: Option<String>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .mb(px(10.))
        .child(this.section(title, false))
        .children(tag.map(|text| {
            div()
                .px(px(7.))
                .py(px(3.))
                .border_1()
                .border_color(rgba(BORDER))
                .rounded(px(4.))
                .text_size(px(10.))
                .text_color(rgba(DIM))
                .child(text)
        }))
        .into_any_element()
}
fn description(text: String) -> AnyElement {
    div()
        .mb(px(14.))
        .text_size(px(12.))
        .line_height(px(19.))
        .text_color(rgba(DIM))
        .child(text)
        .into_any_element()
}
fn usage_card(window: &mapping::Usage, copy: Copy<'_>) -> AnyElement {
    div()
        .min_w_0()
        .p(px(16.))
        .border_1()
        .border_color(rgba(BORDER))
        .rounded(px(10.))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .child(window.title.clone()),
        )
        .child(
            div()
                .mt(px(5.))
                .text_size(px(11.))
                .text_color(rgba(DIM))
                .child(window.duration.clone()),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap(px(7.))
                .mt(px(20.))
                .mb(px(14.))
                .child(
                    div()
                        .text_size(px(if window.lower_bound { 22. } else { 30. }))
                        .line_height(px(if window.lower_bound { 27.5 } else { 30. }))
                        .font_weight(FontWeight::MEDIUM)
                        .child(window.observed.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgba(DIM))
                        .child(copy.text("observed")),
                )
                .when(window.warning, |d| {
                    d.child(
                        div()
                            .ml_auto()
                            .text_color(rgb(0xff8549))
                            .child(icon("alert", 13.)),
                    )
                }),
        )
        .children(window.meter.map(|fraction| {
            div()
                .h(px(4.))
                .w_full()
                .rounded_full()
                .overflow_hidden()
                .bg(rgba(0xffffff1a))
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction))
                        .rounded_full()
                        .bg(rgb(0x99ceff)),
                )
        }))
        .when(window.lower_bound, |d| {
            d.child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(0xff8549))
                    .child(copy.text("modelTotalUnknown")),
            )
        })
        .child(
            div()
                .mt(px(9.))
                .text_size(px(11.))
                .text_color(rgba(DIM))
                .child(window.reference.clone()),
        )
        .children(window.uncertainty.as_ref().map(|text| {
            div()
                .mt(px(5.))
                .text_size(px(11.))
                .line_height(px(17.))
                .text_color(rgb(0xff8549))
                .child(text.clone())
        }))
        .into_any_element()
}
fn reference_card(plan: mapping::Reference, copy: Copy<'_>) -> AnyElement {
    div()
        .min_w_0()
        .p(px(16.))
        .border_1()
        .border_color(rgba(BORDER))
        .rounded(px(10.))
        .when(plan.unavailable, |d| {
            d.bg(rgba(0xffffff08)).text_color(rgba(DIM))
        })
        .child(
            div()
                .mb(px(14.))
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .child(plan.title),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .children(
                    plan.rows
                        .into_iter()
                        .enumerate()
                        .map(|(index, (label, value))| {
                            div()
                                .grid()
                                .grid_cols(2)
                                .gap(px(8.))
                                .text_size(px(12.))
                                .line_height(px(18.))
                                .when(index == 2, |d| {
                                    d.pt(px(10.)).border_t_1().border_color(rgba(BORDER))
                                })
                                .child(div().text_color(rgba(SECONDARY)).child(label))
                                .child(
                                    div()
                                        .text_right()
                                        .when(plan.reference_only, |d| {
                                            d.text_color(rgba(SECONDARY))
                                        })
                                        .child(value),
                                )
                        }),
                ),
        )
        .when(plan.unavailable, |d| {
            d.child(div().text_size(px(16.)).child(copy.text("unknown")))
        })
        .children(plan.notes.into_iter().map(|text| {
            div()
                .mt(px(12.))
                .text_size(px(11.))
                .line_height(px(18.))
                .text_color(rgba(DIM))
                .child(text)
        }))
        .into_any_element()
}
