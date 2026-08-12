//! 端到端：**真实环境变量 → 真实路径推导 → 真实文件搬运**。
//!
//! 单测里测的是 `migrate_file(old, new)` 那层纯逻辑（给两个路径，搬不搬、怎么搬）。
//! 这里补上它**上面**那层——「`HOME` / `USERPROFILE` / `APPDATA` 各是什么 → 老位置和新位置
//! 分别算在哪」，而那才是迁移真正容易错的地方：算错一个字，老用户的文件就搬不过来，
//! 表现是「设置凭空丢了」且不报任何错。
//!
//! **整个文件只放一个 `#[test]`**：这里改的是**进程全局**的环境变量，同一个二进制里的测试
//! 并行跑会互相把对方的设置抹掉。Rust 的集成测试每个文件是独立进程，故与其它测试互不干扰；
//! 但本文件内部必须保持单测试、按阶段顺序走。

use apicase_core::http::ProxyConfig;
use apicase_core::paths::*;

#[test]
fn settings_migrate_from_the_real_legacy_location() {
    let base = std::env::temp_dir().join("apicase-paths-e2e");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("建 fake home");

    // 把这个进程的「主目录」整体挪到临时目录。三平台各自认的变量都要设：
    // 漏掉任何一个，那个平台上就会算到**本机真实的主目录**去——测试跑一次就把用户
    // 自己的 settings.json 搬走了，这是绝不能发生的事。
    std::env::set_var("HOME", &base); // Unix
    std::env::set_var("USERPROFILE", &base); // Windows
    std::env::set_var("APPDATA", base.join("AppData").join("Roaming")); // Windows 的老位置
    std::env::remove_var("XDG_CONFIG_HOME"); // Linux：外部设过就不在 fake home 里了
    std::env::remove_var(HOME_ENV); // 覆盖变量留到最后一段再验

    let new_file = app_settings_file().expect("应能算出新位置");
    let old_file = legacy_app_settings_file().expect("应能算出老位置");

    // 先确认两条路径都落在 fake home 内——否则下面的搬运会动到真实文件
    assert!(new_file.starts_with(&base), "新位置跑到 fake home 外了：{}", new_file.display());
    assert!(old_file.starts_with(&base), "老位置跑到 fake home 外了：{}", old_file.display());
    assert_eq!(new_file, base.join(APP_DIR_NAME).join(SETTINGS_FILE), "新位置就该是 ~/.apicase/settings.json");
    assert_ne!(new_file, old_file);

    // ① 全新用户：两处都没有。不该报错，也不该凭空造出文件
    assert!(!migrate_legacy_settings(), "没有老文件时不该「搬」");
    assert_eq!(load_app_prefs().proxy, None, "没配过 = 跟随系统");
    assert!(!new_file.exists(), "读一下不该顺手创建文件");

    // ② 老用户，桌面端还没跑过（迁移尚未发生）：CLI 必须读得到老位置的代理设置
    std::fs::create_dir_all(old_file.parent().expect("老目录")).expect("建老目录");
    let content = r#"{"theme":"dark","proxy":{"mode":"custom","url":"http://127.0.0.1:7890"}}"#;
    std::fs::write(&old_file, content).expect("写老文件");
    let want = ProxyConfig { mode: "custom".into(), url: Some("http://127.0.0.1:7890".into()) };
    assert_eq!(load_app_prefs().proxy, Some(want.clone()), "迁移前 CLI 就该回落读老位置");

    // ③ 桌面端启动：搬一次
    assert!(migrate_legacy_settings(), "该搬");
    assert_eq!(std::fs::read_to_string(&new_file).expect("新文件应存在"), content, "内容要一字不差");
    assert!(!old_file.exists(), "老文件应已搬走");
    assert!(!old_file.parent().expect("老目录").exists(), "搬空的老目录顺手收拾掉");
    assert_eq!(load_app_prefs().proxy, Some(want.clone()), "迁移前后读到的值必须一模一样");

    // ④ 每次启动都会调，必须幂等
    assert!(!migrate_legacy_settings(), "第二次不该再动");
    assert_eq!(load_app_prefs().proxy, Some(want.clone()));

    // ⑤ 老文件又冒出来（回退过旧版本、或从别处拷了一份）：**新文件优先，绝不覆盖**
    std::fs::create_dir_all(old_file.parent().expect("老目录")).expect("建老目录");
    std::fs::write(&old_file, r#"{"proxy":{"mode":"none"}}"#).expect("写老文件");
    assert!(!migrate_legacy_settings(), "新文件在就不该动");
    assert_eq!(load_app_prefs().proxy, Some(want), "新位置才是权威源");

    // ⑥ APICASE_HOME 覆盖（CI / 容器 / 便携安装）
    let pinned = base.join("钉住的目录");
    std::env::set_var(HOME_ENV, &pinned);
    assert_eq!(app_config_dir().as_deref(), Some(pinned.as_path()));
    assert_eq!(app_settings_file(), Some(pinned.join(SETTINGS_FILE)));
    // 相对路径不算数（只认绝对路径），退回 ~/.apicase
    std::env::set_var(HOME_ENV, "相对路径");
    assert_eq!(app_settings_file(), Some(new_file), "相对路径应按未设置处理");

    let _ = std::fs::remove_dir_all(&base);
}
