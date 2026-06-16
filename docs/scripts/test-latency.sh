#!/bin/bash

# Скрипт для порівняння латентності WebRTC версій

echo "=== Тест латентності WebRTC ==="
echo ""
echo "1. Запускаю сервер..."
cd /home/tarilka0gg/test-websoket-function

# Перевіряємо чи вже запущений
if pgrep -f "test-websoket-function" > /dev/null; then
    echo "⚠️  Сервер вже запущений. Зупиняю..."
    pkill -f "test-websoket-function"
    sleep 1
fi

# Запускаємо сервер
./target/release/test-websoket-function &
SERVER_PID=$!

echo "✓ Сервер запущений (PID: $SERVER_PID)"
echo ""
echo "2. Відкрийте у браузері:"
echo ""
echo "   Оригінальна версія (з лагами):"
echo "   → http://localhost:8080/index.html"
echo ""
echo "   Оптимізована версія (низька латентність):"
echo "   → http://localhost:8080/index-optimized.html"
echo ""
echo "   Фіксована версія (з reconnect логікою):"
echo "   → http://localhost:8080/index-fixed.html"
echo ""
echo "3. Порівняйте метрики у статус-барі:"
echo "   - Латентність (мс) - має бути нижче"
echo "   - Плавність картинки"
echo ""
echo "Натисніть Ctrl+C для зупинки сервера"
echo ""

# Чекаємо на Ctrl+C
trap "echo ''; echo 'Зупиняю сервер...'; kill $SERVER_PID 2>/dev/null; exit 0" INT

wait $SERVER_PID
