<?php
// Keep-alive for the Render free instance. Sends one GET to the fixed health
// URL and returns the result as JSON. Takes no input, so it cannot be used to
// reach other hosts. A lock-free file throttle makes sure visitors and the
// cron job together never hit Render more than once every MIN_INTERVAL s.

declare(strict_types=1);

const TARGET = 'https://spherex-4e2z.onrender.com/api/v1/health';
const MIN_INTERVAL = 240;      // seconds between real upstream requests
const TIMEOUT = 60;            // a sleeping Render instance can take ~50 s

header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-store');

$stateFile = sys_get_temp_dir() . '/spherex_keepalive.json';
$now = time();

$state = null;
$raw = @file_get_contents($stateFile);
if ($raw !== false) {
    $decoded = json_decode($raw, true);
    if (is_array($decoded) && isset($decoded['at'], $decoded['ok'])) {
        $state = $decoded;
    }
}

if ($state !== null && ($now - (int) $state['at']) < MIN_INTERVAL) {
    echo json_encode([
        'ok' => (bool) $state['ok'],
        'status' => $state['status'] ?? 0,
        'ms' => $state['ms'] ?? 0,
        'cached' => true,
        'checked_at' => (int) $state['at'],
    ]);
    exit;
}

$start = microtime(true);
$status = 0;

if (function_exists('curl_init')) {
    $ch = curl_init(TARGET);
    curl_setopt_array($ch, [
        CURLOPT_RETURNTRANSFER => true,
        CURLOPT_FOLLOWLOCATION => false,
        CURLOPT_CONNECTTIMEOUT => 10,
        CURLOPT_TIMEOUT => TIMEOUT,
        CURLOPT_USERAGENT => 'spherex-landing-keepalive/1.0',
    ]);
    curl_exec($ch);
    $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
    curl_close($ch);
} else {
    $ctx = stream_context_create(['http' => [
        'method' => 'GET',
        'timeout' => TIMEOUT,
        'ignore_errors' => true,
        'header' => "User-Agent: spherex-landing-keepalive/1.0\r\n",
    ]]);
    @file_get_contents(TARGET, false, $ctx);
    foreach ($http_response_header ?? [] as $line) {
        if (preg_match('#^HTTP/\S+\s+(\d{3})#', $line, $m)) {
            $status = (int) $m[1];
        }
    }
}

$ms = (int) round((microtime(true) - $start) * 1000);
$ok = $status >= 200 && $status < 300;

@file_put_contents(
    $stateFile,
    json_encode(['at' => $now, 'ok' => $ok, 'status' => $status, 'ms' => $ms]),
    LOCK_EX
);

echo json_encode([
    'ok' => $ok,
    'status' => $status,
    'ms' => $ms,
    'cached' => false,
    'checked_at' => $now,
]);
