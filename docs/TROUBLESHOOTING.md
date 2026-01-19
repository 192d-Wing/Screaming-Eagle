# Troubleshooting Guide

This guide helps diagnose and resolve common issues with Screaming Eagle CDN.

## Table of Contents

- [General Troubleshooting](#general-troubleshooting)
- [Startup Issues](#startup-issues)
- [Performance Problems](#performance-problems)
- [Cache Issues](#cache-issues)
- [Origin Connectivity](#origin-connectivity)
- [Rate Limiting Problems](#rate-limiting-problems)
- [Circuit Breaker Issues](#circuit-breaker-issues)
- [TLS/HTTPS Issues](#tlshttps-issues)
- [Memory Issues](#memory-issues)
- [Network Issues](#network-issues)
- [Monitoring and Debugging](#monitoring-and-debugging)
- [Common Error Messages](#common-error-messages)

## General Troubleshooting

### Step-by-Step Diagnostic Process

1. **Check logs:**
   ```bash
   # If running with systemd
   sudo journalctl -u screaming-eagle -f

   # If running in Docker
   docker logs -f cdn

   # If running directly
   RUST_LOG=debug ./screaming-eagle-cdn
   ```

2. **Check health endpoint:**
   ```bash
   curl http://localhost:8080/_cdn/health
   ```

3. **Check metrics:**
   ```bash
   curl http://localhost:8080/_cdn/metrics
   ```

4. **Check circuit breakers:**
   ```bash
   curl -H "Authorization: Bearer your-token" \
     http://localhost:8080/_cdn/circuit-breakers
   ```

5. **Check origin health:**
   ```bash
   curl -H "Authorization: Bearer your-token" \
     http://localhost:8080/_cdn/origins/health
   ```

## Startup Issues

### CDN Won't Start

**Symptom:** Application exits immediately or fails to start

**Common Causes:**

1. **Configuration file not found:**
   ```
   Error: Configuration file not found: config/cdn.toml
   ```

   **Solution:**
   ```bash
   # Verify file exists
   ls -l config/cdn.toml

   # Specify config path explicitly
   ./screaming-eagle-cdn --config /path/to/cdn.toml
   ```

2. **Invalid configuration:**
   ```
   Error: Invalid configuration: missing required field 'origins'
   ```

   **Solution:**
   - Validate TOML syntax: https://www.toml-lint.com/
   - Check for required fields
   - Review [Configuration Reference](CONFIGURATION.md)

3. **Port already in use:**
   ```
   Error: Address already in use (os error 48)
   ```

   **Solution:**
   ```bash
   # Find what's using the port
   lsof -i :8080

   # Kill the process or change port in config
   [server]
   port = 8081
   ```

4. **Permission denied (port < 1024):**
   ```
   Error: Permission denied (os error 13)
   ```

   **Solution:**
   ```bash
   # Option 1: Use port > 1024
   [server]
   port = 8080

   # Option 2: Run with capabilities (Linux)
   sudo setcap 'cap_net_bind_service=+ep' /usr/local/bin/screaming-eagle-cdn

   # Option 3: Use authbind
   authbind --deep ./screaming-eagle-cdn
   ```

### TLS Certificate Errors

**Symptom:** TLS-related startup failure

**Common Errors:**

1. **Certificate file not found:**
   ```
   Error: TLS certificate not found: /path/to/cert.pem
   ```

   **Solution:**
   ```bash
   # Verify certificate exists
   ls -l /path/to/cert.pem

   # Check permissions
   chmod 644 /path/to/cert.pem
   chmod 600 /path/to/key.pem
   ```

2. **Invalid certificate format:**
   ```
   Error: Invalid TLS certificate format
   ```

   **Solution:**
   ```bash
   # Verify certificate is PEM format
   openssl x509 -in cert.pem -text -noout

   # Convert if needed
   openssl x509 -in cert.der -inform DER -out cert.pem -outform PEM
   ```

3. **Private key mismatch:**
   ```
   Error: Private key does not match certificate
   ```

   **Solution:**
   ```bash
   # Verify certificate and key match
   openssl x509 -noout -modulus -in cert.pem | openssl md5
   openssl rsa -noout -modulus -in key.pem | openssl md5
   # Both should output the same hash
   ```

## Performance Problems

### High Latency

**Symptom:** Slow response times

**Diagnostic Steps:**

1. **Check cache hit ratio:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/stats | jq '.hit_ratio'
   ```

   **Low hit ratio (< 0.7):**
   - Increase cache size
   - Increase TTLs
   - Check if cache-busting queries are hurting hit ratio

2. **Check origin response times:**
   ```bash
   curl http://localhost:8080/_cdn/metrics | grep duration
   ```

   **High origin latency:**
   - Increase origin timeout
   - Check origin server performance
   - Consider adding more origins

3. **Check request coalescing:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/coalesce
   ```

   **High coalescing (> 20%):**
   - Cache is working well
   - Consider cache warming for popular content

### High Memory Usage

**Symptom:** CDN using too much memory

**Diagnostic Steps:**

1. **Check cache size:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/stats | jq '.total_size_bytes'
   ```

2. **Check cache utilization:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/stats | jq '.utilization_percent'
   ```

**Solutions:**

1. **Reduce cache size:**
   ```toml
   [cache]
   max_size_mb = 512  # Lower limit
   max_entry_size_mb = 50
   ```

2. **Reduce worker count:**
   ```toml
   [server]
   workers = 2  # Fewer threads
   ```

3. **Enable cache eviction logging:**
   ```toml
   [logging]
   level = "debug"
   ```

### High CPU Usage

**Symptom:** CPU usage constantly high

**Diagnostic Steps:**

1. **Check request rate:**
   ```bash
   curl http://localhost:8080/_cdn/metrics | grep requests_total
   ```

2. **Check for compression overhead:**
   - Large files being compressed repeatedly
   - Consider caching compressed responses

**Solutions:**

1. **Increase cache hit ratio:**
   - Larger cache size
   - Better TTL configuration

2. **Add more instances:**
   - Horizontal scaling
   - Load balancing

3. **Optimize workers:**
   ```toml
   [server]
   workers = 4  # Match CPU cores
   ```

## Cache Issues

### Cache Not Working

**Symptom:** All requests showing X-Cache: MISS

**Diagnostic Steps:**

1. **Check cache configuration:**
   ```bash
   # Verify cache is enabled and sized
   grep -A5 '\[cache\]' config/cdn.toml
   ```

2. **Check response headers:**
   ```bash
   curl -I http://localhost:8080/example/test.html | grep -i cache
   ```

3. **Check origin Cache-Control:**
   ```bash
   # Origin might be sending no-cache
   curl -I https://origin.example.com/test.html | grep -i cache-control
   ```

**Common Causes:**

1. **Origin sends Cache-Control: no-cache:**
   ```toml
   [cache]
   respect_cache_control = false  # Override origin directives
   ```

2. **Responses too large:**
   ```toml
   [cache]
   max_entry_size_mb = 200  # Increase limit
   ```

3. **Cache size too small:**
   ```toml
   [cache]
   max_size_mb = 4096  # Increase cache size
   ```

### Cache Evicting Too Quickly

**Symptom:** Frequent cache misses for popular content

**Diagnostic Steps:**

1. **Check eviction count:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/stats | jq '.eviction_count'
   ```

2. **Check cache pressure:**
   ```bash
   # High utilization = constant eviction
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/stats | jq '.utilization_percent'
   ```

**Solutions:**

1. **Increase cache size:**
   ```toml
   [cache]
   max_size_mb = 8192
   ```

2. **Increase TTLs:**
   ```toml
   [cache]
   default_ttl_secs = 7200  # 2 hours
   ```

3. **Pre-warm cache:**
   ```bash
   curl -X POST http://localhost:8080/_cdn/warm \
     -H "Authorization: Bearer token" \
     -d '{"urls": ["http://localhost:8080/example/popular.html"]}'
   ```

### Stale Content Being Served

**Symptom:** Old content returned even after origin updated

**Diagnostic Steps:**

1. **Check cache entry:**
   ```bash
   curl -I http://localhost:8080/example/test.html | grep -E 'Age|Date|Cache-Control'
   ```

2. **Check TTL:**
   ```bash
   # Look for max-age in Cache-Control
   curl -I http://localhost:8080/example/test.html | grep Cache-Control
   ```

**Solutions:**

1. **Purge cache:**
   ```bash
   curl -X POST http://localhost:8080/_cdn/purge \
     -H "Authorization: Bearer token" \
     -d '{"key": "/test.html"}'
   ```

2. **Reduce TTL:**
   ```toml
   [cache]
   default_ttl_secs = 300  # 5 minutes
   max_ttl_secs = 3600     # 1 hour
   ```

3. **Use versioned URLs:**
   ```
   # Instead of: /styles.css
   # Use: /styles.css?v=123
   ```

## Origin Connectivity

### Origin Timeout

**Symptom:** 504 Gateway Timeout errors

**Diagnostic Steps:**

1. **Check origin health:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/origins/health
   ```

2. **Test origin directly:**
   ```bash
   time curl https://origin.example.com/test.html
   ```

3. **Check circuit breaker:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/circuit-breakers
   ```

**Solutions:**

1. **Increase timeout:**
   ```toml
   [origins.example]
   url = "https://example.com"
   timeout_secs = 60  # Increase from 30
   ```

2. **Enable stale-if-error:**
   ```toml
   [cache]
   stale_if_error_secs = 3600  # Serve stale on timeout
   ```

3. **Check origin performance:**
   - Origin server overloaded
   - Network issues
   - DNS resolution slow

### Origin Connection Refused

**Symptom:** 502 Bad Gateway errors

**Diagnostic Steps:**

1. **Verify origin URL:**
   ```bash
   curl -v https://origin.example.com
   ```

2. **Check DNS resolution:**
   ```bash
   nslookup origin.example.com
   ```

3. **Check network connectivity:**
   ```bash
   ping origin.example.com
   traceroute origin.example.com
   ```

**Solutions:**

1. **Fix origin URL:**
   ```toml
   [origins.example]
   url = "https://correct-origin.example.com"
   ```

2. **Check firewall rules:**
   ```bash
   # Ensure CDN can reach origin
   telnet origin.example.com 443
   ```

3. **Verify origin is running:**
   ```bash
   # SSH to origin server
   systemctl status web-server
   ```

### Origin SSL/TLS Errors

**Symptom:** SSL certificate verification failed

**Diagnostic Steps:**

1. **Test origin certificate:**
   ```bash
   openssl s_client -connect origin.example.com:443 -servername origin.example.com
   ```

2. **Check certificate validity:**
   ```bash
   echo | openssl s_client -connect origin.example.com:443 2>/dev/null | openssl x509 -noout -dates
   ```

**Solutions:**

1. **Update origin certificate:**
   - Renew expired certificate
   - Fix certificate chain

2. **Allow insecure origins (not recommended for production):**
   ```toml
   [origins.example]
   url = "https://example.com"
   verify_ssl = false  # Only for development
   ```

## Rate Limiting Problems

### Legitimate Requests Being Rate Limited

**Symptom:** 429 Too Many Requests for normal traffic

**Diagnostic Steps:**

1. **Check rate limit configuration:**
   ```bash
   grep -A4 '\[rate_limit\]' config/cdn.toml
   ```

2. **Check client IP:**
   ```bash
   # See which IP is being rate limited
   grep "Rate limit exceeded" /var/log/screaming-eagle.log
   ```

**Solutions:**

1. **Increase rate limit:**
   ```toml
   [rate_limit]
   requests_per_window = 5000  # Increase limit
   window_secs = 60
   burst_size = 200
   ```

2. **Disable rate limiting:**
   ```toml
   [rate_limit]
   enabled = false
   ```

3. **Fix X-Forwarded-For header:**
   ```bash
   # If behind load balancer, ensure X-Forwarded-For is set correctly
   # Load balancer should set this header
   ```

### Rate Limiting Not Working

**Symptom:** Abuse not being blocked

**Diagnostic Steps:**

1. **Verify rate limiting is enabled:**
   ```bash
   grep enabled config/cdn.toml
   ```

2. **Check if IP is being extracted correctly:**
   ```bash
   # Look for client IP in logs
   RUST_LOG=debug ./screaming-eagle-cdn
   ```

**Solutions:**

1. **Enable rate limiting:**
   ```toml
   [rate_limit]
   enabled = true
   ```

2. **Lower threshold:**
   ```toml
   [rate_limit]
   requests_per_window = 100
   window_secs = 60
   burst_size = 10
   ```

## Circuit Breaker Issues

### Circuit Breaker Stuck Open

**Symptom:** 503 errors even though origin is healthy

**Diagnostic Steps:**

1. **Check circuit breaker state:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/circuit-breakers
   ```

2. **Check origin health:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/origins/health
   ```

**Solutions:**

1. **Wait for automatic reset:**
   - Circuit breaker will try half-open after timeout

2. **Reduce reset timeout:**
   ```toml
   [circuit_breaker]
   reset_timeout_secs = 15  # Try sooner
   ```

3. **Restart CDN (last resort):**
   ```bash
   systemctl restart screaming-eagle
   ```

### Circuit Breaker Opening Too Quickly

**Symptom:** Circuit breaker opens for transient issues

**Diagnostic Steps:**

1. **Check failure threshold:**
   ```bash
   grep failure_threshold config/cdn.toml
   ```

2. **Review logs for failures:**
   ```bash
   journalctl -u screaming-eagle | grep -i "origin failure"
   ```

**Solutions:**

1. **Increase failure threshold:**
   ```toml
   [circuit_breaker]
   failure_threshold = 10  # More tolerant
   failure_window_secs = 120
   ```

2. **Increase origin timeout:**
   ```toml
   [origins.example]
   timeout_secs = 60
   ```

## TLS/HTTPS Issues

### Certificate Verification Failed

**Symptom:** Clients getting SSL errors

**Diagnostic Steps:**

1. **Test certificate:**
   ```bash
   openssl s_client -connect cdn.example.com:443 -servername cdn.example.com
   ```

2. **Check certificate chain:**
   ```bash
   openssl x509 -in cert.pem -text -noout | grep -A1 "Issuer"
   ```

**Solutions:**

1. **Use full certificate chain:**
   ```bash
   # Concatenate certificate + intermediates + root
   cat cert.pem intermediate.pem root.pem > fullchain.pem
   ```

   ```toml
   [tls]
   cert_path = "/path/to/fullchain.pem"
   key_path = "/path/to/key.pem"
   ```

2. **Renew expired certificate:**
   ```bash
   certbot renew
   systemctl restart screaming-eagle
   ```

### TLS Handshake Failures

**Symptom:** Some clients can't connect

**Diagnostic Steps:**

1. **Check TLS version support:**
   ```bash
   nmap --script ssl-enum-ciphers -p 443 cdn.example.com
   ```

2. **Test with specific TLS version:**
   ```bash
   openssl s_client -connect cdn.example.com:443 -tls1_2
   openssl s_client -connect cdn.example.com:443 -tls1_3
   ```

**Solutions:**

1. **Update certificate:**
   - Use modern certificate with good compatibility

2. **Check Rustls configuration:**
   - Rustls supports TLS 1.2 and 1.3 by default

## Memory Issues

### Out of Memory

**Symptom:** CDN crashes or gets killed

**Diagnostic Steps:**

1. **Check memory usage:**
   ```bash
   # For systemd
   systemctl status screaming-eagle | grep Memory

   # For Docker
   docker stats cdn

   # For process
   top -p $(pgrep screaming-eagle)
   ```

2. **Check cache size:**
   ```bash
   curl -H "Authorization: Bearer token" \
     http://localhost:8080/_cdn/stats | jq '.total_size_bytes'
   ```

**Solutions:**

1. **Reduce cache size:**
   ```toml
   [cache]
   max_size_mb = 512
   max_entry_size_mb = 50
   ```

2. **Limit container memory:**
   ```bash
   docker run --memory="2g" --memory-swap="2g" cdn
   ```

3. **Add swap space:**
   ```bash
   sudo fallocate -l 4G /swapfile
   sudo chmod 600 /swapfile
   sudo mkswap /swapfile
   sudo swapon /swapfile
   ```

### Memory Leak

**Symptom:** Memory usage grows over time

**Diagnostic Steps:**

1. **Monitor memory over time:**
   ```bash
   watch -n 5 'curl -s http://localhost:8080/_cdn/stats | jq ".total_size_bytes"'
   ```

2. **Check metrics:**
   ```bash
   curl http://localhost:8080/_cdn/metrics | grep memory
   ```

**Solutions:**

1. **Report issue:**
   - This may be a bug
   - Open GitHub issue with reproduction steps

2. **Periodic restart (workaround):**
   ```bash
   # Systemd timer for weekly restart
   sudo systemctl enable screaming-eagle-restart.timer
   ```

## Network Issues

### High Packet Loss

**Symptom:** Intermittent connection failures

**Diagnostic Steps:**

1. **Test network:**
   ```bash
   ping -c 100 origin.example.com | grep loss
   ```

2. **Check MTU:**
   ```bash
   ip link show | grep mtu
   ```

**Solutions:**

1. **Enable retries:**
   ```toml
   [origins.example]
   max_retries = 3
   ```

2. **Adjust MTU:**
   ```bash
   sudo ip link set dev eth0 mtu 1400
   ```

### DNS Resolution Failures

**Symptom:** Cannot resolve origin hostnames

**Diagnostic Steps:**

1. **Test DNS:**
   ```bash
   nslookup origin.example.com
   dig origin.example.com
   ```

2. **Check /etc/resolv.conf:**
   ```bash
   cat /etc/resolv.conf
   ```

**Solutions:**

1. **Use reliable DNS:**
   ```bash
   # /etc/resolv.conf
   nameserver 8.8.8.8
   nameserver 1.1.1.1
   ```

2. **Use IP addresses (workaround):**
   ```toml
   [origins.example]
   url = "https://203.0.113.10"
   host_header = "example.com"
   ```

## Monitoring and Debugging

### Enable Debug Logging

```bash
# Environment variable
RUST_LOG=debug ./screaming-eagle-cdn

# Or in config
[logging]
level = "debug"
```

### Enable Trace Logging

```bash
RUST_LOG=trace ./screaming-eagle-cdn
```

### View Specific Module Logs

```bash
# Only cache module
RUST_LOG=screaming_eagle::cache=debug ./screaming-eagle-cdn

# Multiple modules
RUST_LOG=screaming_eagle::cache=debug,screaming_eagle::origin=trace ./screaming-eagle-cdn
```

### Inspect Cache Contents

```bash
# Get cache stats
curl -H "Authorization: Bearer token" \
  http://localhost:8080/_cdn/stats | jq

# Get cache hit ratio
curl -H "Authorization: Bearer token" \
  http://localhost:8080/_cdn/stats | jq '.hit_ratio'

# Get per-origin stats
curl -H "Authorization: Bearer token" \
  http://localhost:8080/_cdn/stats | jq '.origins'
```

### Monitor in Real-Time

```bash
# Watch metrics
watch -n 1 'curl -s http://localhost:8080/_cdn/metrics | grep requests_total'

# Watch cache stats
watch -n 5 'curl -s -H "Authorization: Bearer token" http://localhost:8080/_cdn/stats | jq ".hit_ratio"'

# Watch logs
tail -f /var/log/screaming-eagle.log
```

### Performance Profiling

```bash
# Use perf (Linux)
perf record -g -p $(pgrep screaming-eagle)
perf report

# Use flamegraph
cargo install flamegraph
sudo flamegraph --pid $(pgrep screaming-eagle)
```

## Common Error Messages

### "Configuration file not found"

**Cause:** Config file doesn't exist or wrong path

**Solution:**
```bash
./screaming-eagle-cdn --config /correct/path/to/cdn.toml
```

### "Address already in use"

**Cause:** Port is already bound

**Solution:**
```bash
# Change port
[server]
port = 8081

# Or kill process using port
lsof -ti:8080 | xargs kill
```

### "Origin 'example' not found"

**Cause:** Origin not configured

**Solution:**
```toml
[origins.example]
url = "https://example.com"
```

### "Rate limit exceeded"

**Cause:** Too many requests from IP

**Solution:**
```toml
[rate_limit]
requests_per_window = 5000  # Increase limit
```

### "Circuit breaker open"

**Cause:** Too many origin failures

**Solution:**
- Wait for automatic reset
- Fix origin issues
- Check origin health endpoint

### "Invalid authentication token"

**Cause:** Wrong or missing Bearer token

**Solution:**
```bash
curl -H "Authorization: Bearer correct-token" http://localhost:8080/_cdn/stats
```

### "TLS certificate verification failed"

**Cause:** Invalid or expired certificate

**Solution:**
- Renew certificate
- Use full certificate chain
- Check certificate matches key

## Getting Help

If you can't resolve the issue:

1. **Check existing issues:** https://github.com/anthropics/screaming-eagle/issues
2. **Gather diagnostic information:**
   - CDN version
   - Configuration file (redact secrets)
   - Error logs
   - Steps to reproduce

3. **Open a new issue:** https://github.com/anthropics/screaming-eagle/issues/new

Include:
- Description of the problem
- Expected behavior
- Actual behavior
- Configuration (sanitized)
- Relevant logs
- System information (OS, Docker version, etc.)
