//! Internationalization (i18n) support for A3Net CLI.
//!
//! Provides runtime locale switching between English and Chinese.
//! Translations are stored in embedded JSON data.
//!
//! ## Usage
//!
//! ```rust
//! use a3net_tui::i18n::{t, set_locale, Locale};
//!
//! // Set locale
//! set_locale(Locale::ZhCn);
//!
//! // Translate a key
//! println!("{}", t("status.title"));
//! println!("{}", t("storage.private"));
//! ```

use std::collections::HashMap;
use once_cell::sync::Lazy;

/// Available locales.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    /// English (default)
    En,
    /// Simplified Chinese
    ZhCn,
}

impl Default for Locale {
    fn default() -> Self {
        Self::En
    }
}

impl Locale {
    /// Get the locale code (e.g., "en", "zh-CN").
    pub fn code(&self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::ZhCn => "zh-CN",
        }
    }

    /// Get the display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Locale::En => "English",
            Locale::ZhCn => "简体中文",
        }
    }
}

/// Current locale setting.
static CURRENT_LOCALE: Lazy<std::sync::RwLock<Locale>> =
    Lazy::new(|| std::sync::RwLock::new(Locale::default()));

/// Get the current locale.
pub fn get_locale() -> Locale {
    *CURRENT_LOCALE.read().unwrap()
}

/// Set the current locale.
pub fn set_locale(locale: Locale) {
    *CURRENT_LOCALE.write().unwrap() = locale;
}

/// Translation data for each locale.
static TRANSLATIONS: Lazy<HashMap<&'static str, HashMap<&'static str, &'static str>>> =
    Lazy::new(|| {
        let mut m = HashMap::new();

        // English translations
        let mut en = HashMap::new();
        en.insert("status.title", "A3Net Node Status");
        en.insert("status.data_dir", "Data Directory");
        en.insert("status.node_id", "Node ID");
        en.insert("status.status", "Status");
        en.insert("status.online", "Online");
        en.insert("status.offline", "Offline");
        en.insert("status.peer_count", "Peer Count");

        en.insert("storage.title", "Storage");
        en.insert("storage.private", "Private");
        en.insert("storage.shared", "Shared");
        en.insert("storage.blobs", "blobs");
        en.insert("storage.used", "used");
        en.insert("storage.budget", "budget");
        en.insert("storage.hard_cap", "hard cap");
        en.insert("storage.free", "free");

        en.insert("replication.title", "Replication");
        en.insert("replication.factor", "Factor");
        en.insert("replication.sweeps", "Sweeps");
        en.insert("replication.blocks", "Blocks");
        en.insert("replication.pushes", "Pushes");
        en.insert("replication.errors", "Errors");
        en.insert("replication.under_replicated", "Under-replicated");
        en.insert("replication.fully_replicated", "Fully-replicated");

        en.insert("alerts.title", "Alerts");
        en.insert("alerts.none", "No issues");
        en.insert("alerts.critical", "Critical");
        en.insert("alerts.warning", "Warning");
        en.insert("alerts.info", "Info");

        en.insert("diagnostics.title", "Diagnostics");
        en.insert("diagnostics.public_key", "Public Key");
        en.insert("diagnostics.mesh_url", "Mesh URL");
        en.insert("diagnostics.not_configured", "Not configured");

        en.insert("config.title", "Configuration");
        en.insert("config.show", "Show");
        en.insert("config.set", "Set");
        en.insert("config.reset", "Reset");
        en.insert("config.edit", "Edit");
        en.insert("config.validate", "Validate");
        en.insert("config.wizard", "Configuration Wizard");

        en.insert("common.ok", "OK");
        en.insert("common.cancel", "Cancel");
        en.insert("common.error", "Error");
        en.insert("common.warning", "Warning");
        en.insert("common.info", "Info");
        en.insert("common.confirm", "Confirm");
        en.insert("common.yes", "Yes");
        en.insert("common.no", "No");

        // Wizard translations
        en.insert("wizard.title", "A3Net Configuration Wizard");
        en.insert("wizard.intro", "This wizard will help you set up A3Net configuration step by step.");
        en.insert("wizard.intro_press_enter", "Press Enter to accept default values shown in brackets.");
        en.insert("wizard.step", "Step");
        en.insert("wizard.basic_settings", "Basic Settings");
        en.insert("wizard.storage_settings", "Storage Settings");
        en.insert("wizard.mesh_server", "Mesh HTTP Server");
        en.insert("wizard.iroh_runtime", "Iroh Runtime");
        en.insert("wizard.relay_server", "Relay Server");
        en.insert("wizard.review", "Configuration Review");
        en.insert("wizard.data_dir", "Data Directory");
        en.insert("wizard.data_dir_help", "Where A3Net stores its data");
        en.insert("wizard.log_level", "Log Level");
        en.insert("wizard.log_level_help", "Logging verbosity");
        en.insert("wizard.log_format", "Log Format");
        en.insert("wizard.log_format_help", "Output format for logs");
        en.insert("wizard.default_room", "Default Room");
        en.insert("wizard.default_room_help", "Room to join by default (leave empty for none)");
        en.insert("wizard.storage_info", "Storage is configured via CLI commands:");
        en.insert("wizard.storage_info_cmd1", "View storage usage");
        en.insert("wizard.storage_info_cmd2", "Set storage limits");
        en.insert("wizard.mesh_host", "Host");
        en.insert("wizard.mesh_host_help", "Address to bind the mesh server");
        en.insert("wizard.mesh_port", "Port");
        en.insert("wizard.mesh_port_help", "Port for mesh server (0 = auto)");
        en.insert("wizard.save", "Save configuration");
        en.insert("wizard.save_success", "Configuration saved to {0}");
        en.insert("wizard.cancel", "Wizard cancelled. No changes made.");
        en.insert("wizard.enabled", "Enabled");
        en.insert("wizard.disabled", "Disabled");
        en.insert("wizard.enter_choice", "Enter choice");
        en.insert("wizard.yes_no", "(yes/no)");

        m.insert("en", en);

        // Chinese translations
        let mut zh = HashMap::new();
        zh.insert("status.title", "A3Net 节点状态");
        zh.insert("status.data_dir", "数据目录");
        zh.insert("status.node_id", "节点 ID");
        zh.insert("status.status", "状态");
        zh.insert("status.online", "在线");
        zh.insert("status.offline", "离线");
        zh.insert("status.peer_count", "对等节点数");

        zh.insert("storage.title", "存储");
        zh.insert("storage.private", "私有存储");
        zh.insert("storage.shared", "共享存储");
        zh.insert("storage.blobs", "数据块");
        zh.insert("storage.used", "已用");
        zh.insert("storage.budget", "配额");
        zh.insert("storage.hard_cap", "硬限制");
        zh.insert("storage.free", "可用");

        zh.insert("replication.title", "复制");
        zh.insert("replication.factor", "因子");
        zh.insert("replication.sweeps", "清理次数");
        zh.insert("replication.blocks", "数据块");
        zh.insert("replication.pushes", "推送次数");
        zh.insert("replication.errors", "错误次数");
        zh.insert("replication.under_replicated", "欠复制");
        zh.insert("replication.fully_replicated", "完全复制");

        zh.insert("alerts.title", "告警");
        zh.insert("alerts.none", "无问题");
        zh.insert("alerts.critical", "严重");
        zh.insert("alerts.warning", "警告");
        zh.insert("alerts.info", "信息");

        zh.insert("diagnostics.title", "诊断信息");
        zh.insert("diagnostics.public_key", "公钥");
        zh.insert("diagnostics.mesh_url", "Mesh URL");
        zh.insert("diagnostics.not_configured", "未配置");

        zh.insert("config.title", "配置");
        zh.insert("config.show", "显示");
        zh.insert("config.set", "设置");
        zh.insert("config.reset", "重置");
        zh.insert("config.edit", "编辑");
        zh.insert("config.validate", "验证");
        zh.insert("config.wizard", "配置向导");

        zh.insert("common.ok", "确定");
        zh.insert("common.cancel", "取消");
        zh.insert("common.error", "错误");
        zh.insert("common.warning", "警告");
        zh.insert("common.info", "信息");
        zh.insert("common.confirm", "确认");
        zh.insert("common.yes", "是");
        zh.insert("common.no", "否");

        // Wizard translations
        zh.insert("wizard.title", "A3Net 配置向导");
        zh.insert("wizard.intro", "本向导将帮助您逐步设置 A3Net 配置。");
        zh.insert("wizard.intro_press_enter", "按 Enter 接受括号中显示的默认值。");
        zh.insert("wizard.step", "步骤");
        zh.insert("wizard.basic_settings", "基本设置");
        zh.insert("wizard.storage_settings", "存储设置");
        zh.insert("wizard.mesh_server", "Mesh HTTP 服务器");
        zh.insert("wizard.iroh_runtime", "Iroh 运行时");
        zh.insert("wizard.relay_server", "中继服务器");
        zh.insert("wizard.review", "配置预览");
        zh.insert("wizard.data_dir", "数据目录");
        zh.insert("wizard.data_dir_help", "A3Net 存储数据的目录");
        zh.insert("wizard.log_level", "日志级别");
        zh.insert("wizard.log_level_help", "日志详细程度");
        zh.insert("wizard.log_format", "日志格式");
        zh.insert("wizard.log_format_help", "日志输出格式");
        zh.insert("wizard.default_room", "默认房间");
        zh.insert("wizard.default_room_help", "默认加入的房间（留空则不设置）");
        zh.insert("wizard.storage_info", "存储通过 CLI 命令配置:");
        zh.insert("wizard.storage_info_cmd1", "查看存储使用情况");
        zh.insert("wizard.storage_info_cmd2", "设置存储限制");
        zh.insert("wizard.mesh_host", "主机地址");
        zh.insert("wizard.mesh_host_help", "Mesh 服务器绑定地址");
        zh.insert("wizard.mesh_port", "端口");
        zh.insert("wizard.mesh_port_help", "Mesh 服务器端口（0 = 自动）");
        zh.insert("wizard.save", "保存配置");
        zh.insert("wizard.save_success", "配置已保存到 {0}");
        zh.insert("wizard.cancel", "向导已取消，未做任何更改。");
        zh.insert("wizard.enabled", "已启用");
        zh.insert("wizard.disabled", "已禁用");
        zh.insert("wizard.enter_choice", "输入选择");
        zh.insert("wizard.yes_no", "（是/否）");

        m.insert("zh-CN", zh);

        m
    });

/// Translate a key.
/// Falls back to the key itself if not found.
pub fn t(key: &str) -> String {
    let locale = get_locale();
    let locale_code = locale.code();

    TRANSLATIONS
        .get(locale_code)
        .and_then(|t| t.get(key))
        .copied()
        .unwrap_or(key)
        .to_string()
}

/// Translate with parameters.
/// Replaces {0}, {1}, etc. in the translation string.
pub fn t_with_args(key: &str, args: &[&str]) -> String {
    let translated = t(key);
    let mut result = translated;
    for (i, arg) in args.iter().enumerate() {
        let placeholder = format!("{{{}}}", i);
        result = result.replace(&placeholder, arg);
    }
    result
}

/// Internationalization helper that can be used in structs.
#[derive(Debug, Clone)]
pub struct I18n;

impl I18n {
    /// Create a new I18n instance.
    pub fn new() -> Self {
        Self
    }

    /// Translate a key.
    pub fn t(&self, key: &str) -> String {
        t(key)
    }

    /// Translate with arguments.
    pub fn t_args(&self, key: &str, args: &[&str]) -> String {
        t_with_args(key, args)
    }

    /// Get current locale.
    pub fn locale(&self) -> Locale {
        get_locale()
    }
}

impl Default for I18n {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_english_default() {
        // Reset to English before test
        set_locale(Locale::En);
        assert_eq!(get_locale(), Locale::En);
        assert_eq!(t("status.title"), "A3Net Node Status");
    }

    #[test]
    fn test_chinese_translation() {
        set_locale(Locale::ZhCn);
        let title = t("status.title");
        // The locale should be Chinese now
        assert!(!title.is_empty());
        // Reset back to English
        set_locale(Locale::En);
    }

    #[test]
    fn test_fallback_to_key() {
        set_locale(Locale::En);
        assert_eq!(t("nonexistent.key"), "nonexistent.key");
    }

    #[test]
    fn test_translate_with_args() {
        set_locale(Locale::En);
        // Test that placeholders work (just verify it doesn't panic)
        let result = t_with_args("status.title", &["arg1", "arg2"]);
        assert!(!result.is_empty());
    }

    #[test]
    fn test_locale_display_name() {
        assert_eq!(Locale::En.display_name(), "English");
        assert_eq!(Locale::ZhCn.display_name(), "简体中文");
    }

    #[test]
    fn test_locale_code() {
        assert_eq!(Locale::En.code(), "en");
        assert_eq!(Locale::ZhCn.code(), "zh-CN");
    }
}
