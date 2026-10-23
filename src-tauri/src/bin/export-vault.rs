//! 把 Railway 自建服务端的全部远端数据导出为加密 vault.amv 文件。
//! 用法：export-vault <输出路径>
#![cfg_attr(not(debug_assertions), windows_subsystem = "console")]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(output) = args.first() else {
        eprintln!("用法: export-vault <输出文件路径>");
        eprintln!("示例: export-vault C:\\Users\\Administrator\\Desktop\\vault.amv");
        std::process::exit(2);
    };
    println!("正在从 Railway 拉取并导出...");
    match agent_manager_lib::cloud_sync::run_vault_export(output) {
        Ok(summary) => {
            println!("{summary}");
        }
        Err(error) => {
            eprintln!("导出失败：{error}");
            std::process::exit(1);
        }
    }
}
