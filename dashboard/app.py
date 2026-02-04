#!/usr/bin/env python3
"""
MEV101 Dashboard - Real-time monitoring for Artemis-mode MEV bot
Parses log file and displays live statistics via web UI
"""

import os
import re
import time
from collections import deque
from datetime import datetime
from flask import Flask, render_template, jsonify

app = Flask(__name__)

# Configuration
LOG_FILE = os.environ.get('MEV_LOG_FILE', '/tmp/mev.log')
MAX_LOG_LINES = 100
STALE_THRESHOLD = 60  # seconds

# Global state for parsed stats
stats = {
    'bot_status': 'unknown',
    'current_block': None,
    'base_fee': None,
    'base_fee_gwei': None,
    'events_processed': 0,
    'actions_generated': 0,
    'last_update': None,
    'log_file_mtime': None,
    'uptime_start': None,
    'collectors': {
        'mempool': {'status': 'unknown', 'endpoint': None},
        'block': {'status': 'unknown', 'endpoint': None},
        'swap_event': {'status': 'unknown', 'endpoint': None},
    },
    'strategies': {
        'arbitrage': {'active': False, 'pairs': 0},
        'sandwich': {'active': False, 'routers': 0},
        'liquidation': {'active': False, 'accounts': 0},
    },
    'actions': {
        'arbitrage': {'count': 0, 'profit_eth': 0.0, 'last_block': None},
        'sandwich': {'count': 0, 'profit_eth': 0.0, 'last_block': None},
        'liquidation': {'count': 0, 'profit_eth': 0.0, 'last_block': None},
    },
    'opportunities': {
        'arbitrage': {'found': 0, 'executed': 0, 'failed': 0},
        'sandwich': {'found': 0, 'executed': 0, 'failed': 0},
        'liquidation': {'found': 0, 'executed': 0, 'failed': 0},
    },
    'wallet': {
        'address': None,
        'balance': None,
    },
    'gas': {
        'total_spent': 0.0,
        'avg_gas_price': None,
    },
    'log_lines': deque(maxlen=MAX_LOG_LINES),
}

# Regex patterns for log parsing
PATTERNS = {
    'block': re.compile(r'BlockCollector: New block (\d+)(?: \(base_fee: (?:Some\()?(\d+)\)?)?'),
    'mempool_connected': re.compile(r'MempoolCollector: Connected to (wss?://[^\s]+)'),
    'mempool_disconnected': re.compile(r'MempoolCollector: Disconnected'),
    'block_collector_connected': re.compile(r'BlockCollector: Connected to (wss?://[^\s]+)'),
    'swap_event_connected': re.compile(r'SwapEventCollector: Connected to (wss?://[^\s]+)'),
    'engine_stats': re.compile(r'Engine stats: (\d+) events processed, (\d+) actions generated'),
    'arbitrage_strategy': re.compile(r'ArbitrageStrategy started: monitoring (\d+) pairs'),
    'sandwich_strategy': re.compile(r'SandwichStrategy started: monitoring (\d+) routers'),
    'liquidation_strategy': re.compile(r'LiquidationStrategy started: watching (\d+) accounts'),
    'executor_address': re.compile(r'Executor address: (0x[a-fA-F0-9]{40})'),
    'balance': re.compile(r'Balance: ([\d.]+) ETH'),
    'startup': re.compile(r'Starting MEV bot|Artemis initialized|Engine started'),
    'shutdown': re.compile(r'Shutting down|Engine stopped'),
    # Action patterns
    'arb_opportunity': re.compile(r'Arbitrage opportunity found.*profit[:\s]*([\d.]+)\s*ETH', re.IGNORECASE),
    'arb_executed': re.compile(r'Arbitrage (?:executed|success|submitted).*block[:\s#]*(\d+)?.*profit[:\s]*([\d.]+)?\s*ETH?', re.IGNORECASE),
    'arb_action': re.compile(r'ArbitrageStrategy.*action|Arbitrage.*bundle|arb.*submitted', re.IGNORECASE),
    'sandwich_opportunity': re.compile(r'Sandwich opportunity found.*profit[:\s]*([\d.]+)?\s*ETH?', re.IGNORECASE),
    'sandwich_executed': re.compile(r'Sandwich (?:executed|success|submitted).*block[:\s#]*(\d+)?.*profit[:\s]*([\d.]+)?\s*ETH?', re.IGNORECASE),
    'sandwich_action': re.compile(r'SandwichStrategy.*action|Sandwich.*bundle|sandwich.*submitted', re.IGNORECASE),
    'liquidation_opportunity': re.compile(r'Liquidation opportunity found.*profit[:\s]*([\d.]+)?\s*ETH?', re.IGNORECASE),
    'liquidation_executed': re.compile(r'Liquidation (?:executed|success|submitted).*block[:\s#]*(\d+)?.*profit[:\s]*([\d.]+)?\s*ETH?', re.IGNORECASE),
    'liquidation_action': re.compile(r'LiquidationStrategy.*action|Liquidation.*bundle|liquidation.*submitted', re.IGNORECASE),
    'action_failed': re.compile(r'(Arbitrage|Sandwich|Liquidation).*(?:failed|reverted|error)', re.IGNORECASE),
    'gas_spent': re.compile(r'gas (?:spent|used|cost)[:\s]*([\d.]+)\s*(?:ETH|gwei)?', re.IGNORECASE),
}


def get_log_level(line):
    """Determine log level from line content"""
    line_lower = line.lower()
    if 'error' in line_lower or 'failed' in line_lower or 'reverted' in line_lower:
        return 'error'
    elif 'warn' in line_lower:
        return 'warn'
    elif 'debug' in line_lower or 'trace' in line_lower:
        return 'debug'
    elif 'opportunity' in line_lower or 'profit' in line_lower:
        return 'success'
    else:
        return 'info'


def parse_log_line(line):
    """Parse a single log line and update stats"""
    global stats

    line = line.strip()
    if not line:
        return

    # Add to log buffer with level
    log_entry = {
        'time': datetime.now().strftime('%H:%M:%S'),
        'level': get_log_level(line),
        'message': line[:500],  # Truncate very long lines
    }
    stats['log_lines'].append(log_entry)
    stats['last_update'] = datetime.now().isoformat()

    # Check for block updates
    match = PATTERNS['block'].search(line)
    if match:
        stats['current_block'] = int(match.group(1))
        if match.group(2):
            base_fee = int(match.group(2))
            stats['base_fee'] = base_fee
            stats['base_fee_gwei'] = round(base_fee / 1e9, 2)
        stats['bot_status'] = 'running'
        stats['collectors']['block']['status'] = 'connected'
        return

    # Check for engine stats
    match = PATTERNS['engine_stats'].search(line)
    if match:
        stats['events_processed'] = int(match.group(1))
        stats['actions_generated'] = int(match.group(2))
        stats['bot_status'] = 'running'
        return

    # Check for mempool collector
    match = PATTERNS['mempool_connected'].search(line)
    if match:
        stats['collectors']['mempool']['status'] = 'connected'
        stats['collectors']['mempool']['endpoint'] = match.group(1)
        stats['bot_status'] = 'running'
        return

    if PATTERNS['mempool_disconnected'].search(line):
        stats['collectors']['mempool']['status'] = 'disconnected'
        return

    # Check for block collector connection
    match = PATTERNS['block_collector_connected'].search(line)
    if match:
        stats['collectors']['block']['status'] = 'connected'
        stats['collectors']['block']['endpoint'] = match.group(1)
        return

    # Check for swap event collector
    match = PATTERNS['swap_event_connected'].search(line)
    if match:
        stats['collectors']['swap_event']['status'] = 'connected'
        stats['collectors']['swap_event']['endpoint'] = match.group(1)
        return

    # Check for strategies
    match = PATTERNS['arbitrage_strategy'].search(line)
    if match:
        stats['strategies']['arbitrage']['active'] = True
        stats['strategies']['arbitrage']['pairs'] = int(match.group(1))
        if not stats['uptime_start']:
            stats['uptime_start'] = datetime.now().isoformat()
        return

    match = PATTERNS['sandwich_strategy'].search(line)
    if match:
        stats['strategies']['sandwich']['active'] = True
        stats['strategies']['sandwich']['routers'] = int(match.group(1))
        return

    match = PATTERNS['liquidation_strategy'].search(line)
    if match:
        stats['strategies']['liquidation']['active'] = True
        stats['strategies']['liquidation']['accounts'] = int(match.group(1))
        return

    # Check for wallet info
    match = PATTERNS['executor_address'].search(line)
    if match:
        stats['wallet']['address'] = match.group(1)
        return

    match = PATTERNS['balance'].search(line)
    if match:
        stats['wallet']['balance'] = match.group(1)
        return

    # Check for startup/shutdown
    if PATTERNS['startup'].search(line):
        stats['bot_status'] = 'running'
        if not stats['uptime_start']:
            stats['uptime_start'] = datetime.now().isoformat()
        return

    if PATTERNS['shutdown'].search(line):
        stats['bot_status'] = 'stopped'
        return

    # --- Action tracking ---

    # Arbitrage opportunities
    match = PATTERNS['arb_opportunity'].search(line)
    if match:
        stats['opportunities']['arbitrage']['found'] += 1
        return

    # Arbitrage executed
    match = PATTERNS['arb_executed'].search(line)
    if match or PATTERNS['arb_action'].search(line):
        stats['actions']['arbitrage']['count'] += 1
        stats['opportunities']['arbitrage']['executed'] += 1
        if match and match.group(1):
            stats['actions']['arbitrage']['last_block'] = int(match.group(1))
        if match and match.group(2):
            stats['actions']['arbitrage']['profit_eth'] += float(match.group(2))
        return

    # Sandwich opportunities
    match = PATTERNS['sandwich_opportunity'].search(line)
    if match:
        stats['opportunities']['sandwich']['found'] += 1
        return

    # Sandwich executed
    match = PATTERNS['sandwich_executed'].search(line)
    if match or PATTERNS['sandwich_action'].search(line):
        stats['actions']['sandwich']['count'] += 1
        stats['opportunities']['sandwich']['executed'] += 1
        if match and match.group(1):
            stats['actions']['sandwich']['last_block'] = int(match.group(1))
        if match and match.group(2):
            stats['actions']['sandwich']['profit_eth'] += float(match.group(2))
        return

    # Liquidation opportunities
    match = PATTERNS['liquidation_opportunity'].search(line)
    if match:
        stats['opportunities']['liquidation']['found'] += 1
        return

    # Liquidation executed
    match = PATTERNS['liquidation_executed'].search(line)
    if match or PATTERNS['liquidation_action'].search(line):
        stats['actions']['liquidation']['count'] += 1
        stats['opportunities']['liquidation']['executed'] += 1
        if match and match.group(1):
            stats['actions']['liquidation']['last_block'] = int(match.group(1))
        if match and match.group(2):
            stats['actions']['liquidation']['profit_eth'] += float(match.group(2))
        return

    # Failed actions
    match = PATTERNS['action_failed'].search(line)
    if match:
        action_type = match.group(1).lower()
        if action_type in stats['opportunities']:
            stats['opportunities'][action_type]['failed'] += 1
        return

    # Gas tracking
    match = PATTERNS['gas_spent'].search(line)
    if match:
        try:
            stats['gas']['total_spent'] += float(match.group(1))
        except ValueError:
            pass
        return


def reset_stats():
    """Reset accumulating stats"""
    stats['actions'] = {
        'arbitrage': {'count': 0, 'profit_eth': 0.0, 'last_block': None},
        'sandwich': {'count': 0, 'profit_eth': 0.0, 'last_block': None},
        'liquidation': {'count': 0, 'profit_eth': 0.0, 'last_block': None},
    }
    stats['opportunities'] = {
        'arbitrage': {'found': 0, 'executed': 0, 'failed': 0},
        'sandwich': {'found': 0, 'executed': 0, 'failed': 0},
        'liquidation': {'found': 0, 'executed': 0, 'failed': 0},
    }
    stats['gas'] = {'total_spent': 0.0, 'avg_gas_price': None}


def read_log_file():
    """Read and parse the entire log file"""
    global stats

    if not os.path.exists(LOG_FILE):
        stats['bot_status'] = 'no_log_file'
        return

    try:
        mtime = os.path.getmtime(LOG_FILE)
        stats['log_file_mtime'] = datetime.fromtimestamp(mtime).isoformat()

        with open(LOG_FILE, 'r') as f:
            # Read last 1000 lines to avoid memory issues with huge logs
            lines = deque(f, maxlen=1000)

        # Clear and rebuild
        stats['log_lines'].clear()
        reset_stats()

        for line in lines:
            parse_log_line(line)

        # Check if log file is stale
        if stats['bot_status'] == 'running':
            if time.time() - mtime > STALE_THRESHOLD:
                stats['bot_status'] = 'stale'

    except Exception as e:
        stats['bot_status'] = f'error: {str(e)}'


@app.route('/')
def index():
    """Serve the dashboard UI"""
    read_log_file()
    return render_template('index.html', stats=stats)


@app.route('/api/stats')
def api_stats():
    """JSON API endpoint for stats"""
    read_log_file()

    # Calculate totals
    total_actions = sum(a['count'] for a in stats['actions'].values())
    total_profit = sum(a['profit_eth'] for a in stats['actions'].values())
    total_opportunities = sum(o['found'] for o in stats['opportunities'].values())

    return jsonify({
        'bot_status': stats['bot_status'],
        'current_block': stats['current_block'],
        'base_fee': stats['base_fee'],
        'base_fee_gwei': stats['base_fee_gwei'],
        'events_processed': stats['events_processed'],
        'actions_generated': stats['actions_generated'],
        'last_update': stats['last_update'],
        'log_file_mtime': stats['log_file_mtime'],
        'uptime_start': stats['uptime_start'],
        'collectors': stats['collectors'],
        'strategies': stats['strategies'],
        'wallet': stats['wallet'],
        'actions': stats['actions'],
        'opportunities': stats['opportunities'],
        'gas': stats['gas'],
        'totals': {
            'actions': total_actions,
            'profit_eth': round(total_profit, 6),
            'opportunities': total_opportunities,
        }
    })


@app.route('/api/logs')
def api_logs():
    """JSON API endpoint for log lines"""
    read_log_file()
    return jsonify({
        'logs': list(stats['log_lines']),
    })


if __name__ == '__main__':
    print(f"MEV101 Dashboard starting...")
    print(f"Monitoring log file: {LOG_FILE}")
    print(f"Open http://127.0.0.1:8080 in your browser")
    app.run(host='0.0.0.0', port=8080, debug=False)
