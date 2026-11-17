@echo off
chcp 65001 >nul
setlocal

echo [1/4] 检查 Rust 工具链...
where cargo >nul 2>nul
if errorlevel 1 (
    echo [错误] 未检测到 cargo，请先安装 Rust (https://rustup.rs) 与 Visual Studio C++ 构建工具。
    exit /b 1
)

echo [2/4] 运行核心调度、配置校验与 NTP 解析单元测试...
cargo test --bin HalfHourChime
if errorlevel 1 (
    echo [错误] 单元测试未通过，已中止打包。
    exit /b 1
)

echo [3/4] 编译 Windows x86_64 静态链接单文件发布版 (Release)...
cargo build --release --target x86_64-pc-windows-msvc
if errorlevel 1 (
    echo [错误] Release 编译失败。
    exit /b 1
)

echo [4/4] 归档单一可执行交付物与按天日志目录...
if not exist "dist" mkdir "dist"
if not exist "dist\logs" mkdir "dist\logs"
copy /Y "target\x86_64-pc-windows-msvc\release\HalfHourChime.exe" "dist\HalfHourChime.exe" >nul

echo.
echo [成功] 已生成单一可执行文件：dist\HalfHourChime.exe
echo        按天归档日志目录已就绪：dist\logs\（half_hour_chime_YYYY-MM-DD.log）
echo        可直接复制 dist 目录至终端双击运行，无需安装任何运行库。
endlocal
