#!/bin/bash
# ============================================================
#  跑跑看：在 Mac 上重現「HPCC 壅塞控制」論文的實驗（縮小版）
#
#  這個檔案在 Finder 裡雙擊就會打開「終端機」來執行。
#  它會做三件事：
#    1. 用 Rust 編譯模擬器（第一次大約 30 秒，之後幾乎不用等）。
#    2. 執行 `make quick`：在一個 32 台主機的小型資料中心網路裡，
#       讓 HPCC、DCQCN、DCTCP 這幾種「壅塞控制」演算法各跑一次，
#       另外再跑論文裡的幾個小實驗（16 對 1 湧入、長短流、公平分享）。
#       整組大約一分鐘。
#    3. 把結果畫成圖，做成一個網頁，用瀏覽器打開。
#
#  需要先裝好：
#    Xcode Command Line Tools（提供 make、python3）：xcode-select --install
#    Rust（提供 cargo）：到 https://rustup.rs 照指示安裝
#  第一次執行時會在專案資料夾裡建立 .venv（Python 虛擬環境）並下載
#  畫圖用的 matplotlib，所以需要網路。
# ============================================================

# 先切換到這個檔案所在的資料夾。資料夾名稱有空白和中文，
# 所以 "$(dirname "$0")" 一定要用雙引號包起來。
cd "$(dirname "$0")" || exit 1

pause_and_exit() {
  read -r -p "按 Enter 關閉視窗..."
  exit "$1"
}

# Homebrew 安裝的 rustup 不一定在 PATH 裡，先幫忙加上。
if ! command -v cargo >/dev/null 2>&1; then
  for d in "$HOME/.cargo/bin" /opt/homebrew/opt/rustup/bin; do
    if [ -x "$d/cargo" ]; then
      export PATH="$d:$PATH"
      break
    fi
  done
fi

# 檢查需要的工具有沒有裝
for tool in make python3 cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "找不到 $tool。"
    echo "  make、python3：在終端機執行 xcode-select --install"
    echo "  cargo（Rust）：打開 https://rustup.rs ，照網頁上的一行指令安裝"
    pause_and_exit 1
  fi
done

echo "== 1/3 編譯模擬器（Rust，release 模式）..."
if ! make -s build; then
  echo "編譯失敗，上面的訊息會說明原因。"
  pause_and_exit 1
fi

echo
echo "== 2/3 執行縮小版實驗（make quick，最多同時用 4 個 CPU 核心）"
echo "   每一行是一次模擬：哪個演算法、跑了幾個 flow、有沒有掉封包（drops）、"
echo "   PFC 暫停了幾次（pause frames）、95% 的封包來回時間（rtt p95）。"
echo
if ! make -s quick; then
  echo "實驗或畫圖失敗，上面的訊息會說明原因。"
  pause_and_exit 1
fi

echo
echo "== 3/3 打開結果網頁"
open "results/quick/index.html"
echo "已用瀏覽器打開 results/quick/index.html。"
echo "（這是縮小版的結果。論文規模 320 台主機的完整結果在 docs/results.html，"
echo "  要重跑完整版請在終端機執行 make repro，大約需要 7 分鐘。）"
pause_and_exit 0
