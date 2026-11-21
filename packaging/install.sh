#!/bin/sh
# Bootstrap the approved native package; lifecycle and registration stay in the manager.
set -eu

usage() {
    printf '%s\n' '用法：install.sh --harness codex|workbuddy [--client "/path/to/codex"] [--workbuddy-app "/path/to/WorkBuddy AI.app"]'
}
fail() { printf '安装未完成：%s\n' "$*" >&2; exit 1; }
native() {
    if ! "$@" > "$work/native.out" 2>&1; then
        cat "$work/native.out" >&2
        fail '请处理上面的原始错误后再运行；已有文件保持。'
    fi
}
main() {
    version='0.1.0-wb1.4'
    digest='2315b725f19621135e138ff8b161637f59c0f2c21a2a9c013f6b6c9695c72994'
    url="https://github.com/Smarpi-clever/Tokemula-Image-Connector/releases/download/v$version/tokemula-image-connector-$version-macos-arm64.tar.gz"
    harness=''; client=''; workbuddy_app=''
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --harness|--client|--workbuddy-app)
                [ "$#" -ge 2 ] && [ -n "$2" ] || fail "$1 缺少值。"
                case "$1" in
                    --harness) harness=$2 ;;
                    --client) client=$2 ;;
                    --workbuddy-app) workbuddy_app=$2 ;;
                esac
                shift 2 ;;
            --help|-h) usage; exit 0 ;;
            *) usage >&2; fail "未知参数：$1" ;;
        esac
    done
    case "$harness" in codex|workbuddy) ;; *) usage >&2; fail '请选择 codex 或 workbuddy。' ;; esac
    [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || fail '此安装入口仅支持 macOS Apple Silicon（M 系列）。'
    [ -n "${HOME:-}" ] || fail '无法定位用户目录 HOME。'
    [ "$harness" = codex ] || [ -z "$client" ] || fail '--client 仅用于 Codex。'
    [ "$harness" = workbuddy ] || [ -z "$workbuddy_app" ] || fail '--workbuddy-app 仅用于 WorkBuddy。'
    for tool in curl shasum tar mktemp plutil; do
        command -v "$tool" >/dev/null 2>&1 || fail "缺少 macOS 工具：${tool}。"
    done
    if [ "$harness" = codex ] && [ -z "$client" ]; then
        client=$(command -v codex || true)
        if [ -z "$client" ]; then
            for app in "$HOME/Applications/ChatGPT.app" /Applications/ChatGPT.app "$HOME/Applications/Codex.app" /Applications/Codex.app; do
                candidate="$app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"
                if [ -x "$candidate" ]; then client=$candidate; break; fi
            done
        fi
        [ -n "$client" ] || fail '未找到 Codex CLI。请安装 Codex App，或用 --client 指定 App 内 codex 程序的完整路径。'
    fi
    root="$HOME/.local/share/tokemula-image-connector"
    manager="$root/bin/tokemula-image"
    if [ -e "$root/transaction.json" ]; then
        fail "上次安装需恢复。请先运行：\"$root/recover\" recover --install-root \"$root\""
    fi
    work=$(mktemp -d "${TMPDIR:-/tmp}/tokemula-image-install.XXXXXX")
    trap 'rm -r "$work"' EXIT
    trap 'exit 1' HUP INT TERM
    if [ -e "$root/installation.json" ]; then
        current=$(plutil -extract current raw -expect string -o - "$root/installation.json") || fail '已有安装记录无法读取，请使用原恢复入口。'
        product=$(plutil -extract product raw -expect string -o - "$root/installation.json") || fail '已有安装记录无法读取，请使用原恢复入口。'
        [ "$product" = tokemula-image-connector ] || fail '安装目录已有其他内容，请保留原目录并检查。'
        [ "$current" = "$version" ] || fail "已安装 ${current}；此入口安装 ${version}，不会自动升级或降级，请使用原版本管理入口。"
        [ -x "$manager" ] || fail '已有安装的管理程序缺失，请使用原恢复入口。'
        [ "$("$manager" --version)" = "tokemula-image $version" ] || fail '已有程序与安装记录不一致，请使用原恢复入口。'
        native "$manager" health
        printf '复用已安装的 Tokemula %s。\n' "$version"
    else
        printf '正在下载 Tokemula %s…\n' "$version"
        curl --fail --location --silent --show-error --proto '=https' --proto-redir '=https' --tlsv1.2 "$url" -o "$work/release.tar.gz"
        actual=$(shasum -a 256 "$work/release.tar.gz")
        [ "${actual%% *}" = "$digest" ] || fail '下载文件校验失败，未运行程序。'
        mkdir "$work/package"
        tar -xzf "$work/release.tar.gz" -C "$work/package"
        printf '%s\n' 'UHyBPGblmoIevgxFsMjCwlw6P9aokVhX41+k63HyLfc=' > "$work/trust-public-key.txt"
        native "$work/package/tokemula-image" setup --source "$work/package" --trust-key "$work/trust-public-key.txt"
    fi
    set -- "$manager" install --harness "$harness" --scope user
    if [ -n "$client" ]; then set -- "$@" --client "$client"; fi
    if [ -n "$workbuddy_app" ]; then set -- "$@" --workbuddy-app "$workbuddy_app"; fi
    native "$@"
    if grep -Eq '"status"[[:space:]]*:[[:space:]]*"awaiting_confirmation"' "$work/native.out"; then
        printf '%s\n' '程序与 Skill 已安装；请在 WorkBuddy 中对 tokemula-image 点击“信任”。'
    elif grep -Eq '"status"[[:space:]]*:[[:space:]]*"(installed|already_installed)"' "$work/native.out"; then
        printf '已为 %s 注册 Tokemula MCP 和 Skill。\n' "$harness"
    else
        cat "$work/native.out" >&2
        fail '客户端返回了其他状态，请按上面的结果处理。'
    fi
    printf '%s\n' '重新打开客户端并新建任务，发送“配置 Tokemula 图片服务”，按返回入口在本机页面填写 Key。' '已有 Key 和默认设置继续沿用；系统及客户端信任由你确认。'
}
main "$@"
