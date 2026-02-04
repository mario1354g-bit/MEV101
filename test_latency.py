#!/usr/bin/env python3
"""Latency test for MEV bot infrastructure"""

import time
import asyncio
import json
import websockets
import requests
import os

# Load config
with open('.env') as f:
    for line in f:
        if '=' in line:
            k, v = line.strip().split('=', 1)
            os.environ[k] = v

WS_URL = os.environ.get('ETH_WS_URL')
RPC_URL = os.environ.get('ETH_RPC_URL')

async def test_websocket_block_latency():
    """Test how fast we receive new blocks via WebSocket"""
    print("\n=== WebSocket Block Latency Test ===")
    print(f"Connecting to {WS_URL[:50]}...")
    
    try:
        async with websockets.connect(WS_URL) as ws:
            # Subscribe to new blocks
            await ws.send(json.dumps({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_subscribe",
                "params": ["newHeads"]
            }))
            
            resp = await ws.recv()
            print(f"Subscribed: {resp[:80]}...")
            
            print("Waiting for 3 blocks to measure latency...")
            latencies = []
            
            for i in range(3):
                # Get block notification
                start = time.time()
                msg = await asyncio.wait_for(ws.recv(), timeout=30)
                ws_time = time.time()
                
                data = json.loads(msg)
                block_num = int(data['params']['result']['number'], 16)
                block_time = int(data['params']['result']['timestamp'], 16)
                
                # Compare to block timestamp
                latency_ms = (ws_time - block_time) * 1000
                latencies.append(latency_ms)
                
                print(f"  Block {block_num}: {latency_ms:.0f}ms from block timestamp")
            
            avg = sum(latencies) / len(latencies)
            print(f"\n  Average block reception latency: {avg:.0f}ms")
            return avg
            
    except Exception as e:
        print(f"Error: {e}")
        return None

def test_rpc_latency():
    """Test RPC call latency"""
    print("\n=== RPC Latency Test ===")
    
    latencies = []
    for i in range(5):
        start = time.time()
        resp = requests.post(RPC_URL, json={
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_blockNumber",
            "params": []
        })
        elapsed = (time.time() - start) * 1000
        latencies.append(elapsed)
        print(f"  eth_blockNumber: {elapsed:.1f}ms")
    
    avg = sum(latencies) / len(latencies)
    print(f"\n  Average RPC latency: {avg:.1f}ms")
    return avg

def test_flashbots_relay_latency():
    """Test Flashbots relay connectivity"""
    print("\n=== Flashbots Relay Latency Test ===")
    
    latencies = []
    for endpoint in [
        "https://relay.flashbots.net",
        "https://rpc.titanbuilder.xyz",
        "https://rsync-builder.xyz",
    ]:
        try:
            start = time.time()
            resp = requests.get(f"{endpoint}", timeout=5)
            elapsed = (time.time() - start) * 1000
            latencies.append((endpoint, elapsed))
            print(f"  {endpoint}: {elapsed:.1f}ms")
        except Exception as e:
            print(f"  {endpoint}: ERROR - {e}")
    
    return latencies

def check_instance_location():
    """Check AWS instance location"""
    print("\n=== Instance Location ===")
    try:
        # AWS metadata endpoint
        resp = requests.get("http://169.254.169.254/latest/meta-data/placement/region", timeout=2)
        region = resp.text
        print(f"  AWS Region: {region}")
        
        resp = requests.get("http://169.254.169.254/latest/meta-data/placement/availability-zone", timeout=2)
        az = resp.text
        print(f"  Availability Zone: {az}")
        
        # Optimal regions for MEV:
        print(f"\n  Note: Flashbots relays are in us-east / eu-west")
        print(f"  Current region: {region}")
        if "us-east" in region or "eu-west" in region:
            print(f"  ✓ Good location for MEV!")
        else:
            print(f"  ⚠ Consider us-east-1 or eu-west-1 for lower latency")
            
    except:
        print("  Not running on AWS or metadata unavailable")

if __name__ == "__main__":
    print("=" * 50)
    print("MEV Latency Test")
    print("=" * 50)
    
    check_instance_location()
    test_rpc_latency()
    test_flashbots_relay_latency()
    
    # Run async WebSocket test
    asyncio.run(test_websocket_block_latency())
    
    print("\n" + "=" * 50)
    print("Summary:")
    print("  - Block latency <500ms: Good")
    print("  - Block latency <200ms: Excellent")
    print("  - RPC latency <50ms: Good")
    print("  - Flashbots <100ms: Competitive")
    print("=" * 50)
