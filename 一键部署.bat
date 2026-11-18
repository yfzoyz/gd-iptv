@echo off
chcp 65001 >nul
title 中兴 AX5400 Pro+ 广东电信 IPTV 一键部署程序
echo 正在启动部署程序，请稍候...
python "%~dp0一键部署.py"
pause
