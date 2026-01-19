# Deployment Guide

This guide covers deploying Screaming Eagle CDN in various environments, from development to production.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Deployment Methods](#deployment-methods)
  - [Docker (Recommended)](#docker-recommended)
  - [Docker Compose](#docker-compose)
  - [Kubernetes](#kubernetes)
  - [Bare Metal](#bare-metal)
  - [Cloud Platforms](#cloud-platforms)
- [Configuration](#configuration)
- [TLS/HTTPS Setup](#tlshttps-setup)
- [Monitoring Setup](#monitoring-setup)
- [High Availability](#high-availability)
- [Production Checklist](#production-checklist)
- [Scaling Strategies](#scaling-strategies)

## Prerequisites

### System Requirements

**Minimum:**
- CPU: 2 cores
- RAM: 512 MB
- Disk: 1 GB for application + cache size
- OS: Linux (Alpine, Ubuntu, Debian, RHEL)

**Recommended for Production:**
- CPU: 4+ cores
- RAM: 4 GB+ (depends on cache size)
- Disk: SSD with cache size + 10 GB
- OS: Linux with kernel 4.9+

### Software Requirements

**For Docker Deployment:**
- Docker 20.10+
- Docker Compose 2.0+ (optional)

**For Bare Metal:**
- Rust 1.75+ (for building)
- OpenSSL or Rustls dependencies

**For Kubernetes:**
- Kubernetes 1.20+
- kubectl configured
- Helm 3+ (optional)

## Deployment Methods

### Docker (Recommended)

Docker is the recommended deployment method for ease and consistency.

#### Build the Image

```bash
cd /path/to/Screaming-Eagle
docker build -t screaming-eagle-cdn:latest .
```

The Dockerfile uses multi-stage builds for minimal image size (~20 MB).

#### Run the Container

**Basic Run:**

```bash
docker run -d \
  --name cdn \
  -p 8080:8080 \
  -v $(pwd)/config:/app/config:ro \
  screaming-eagle-cdn:latest
```

**With Resource Limits:**

```bash
docker run -d \
  --name cdn \
  -p 8080:8080 \
  -v $(pwd)/config:/app/config:ro \
  --memory="2g" \
  --cpus="2.0" \
  --restart=unless-stopped \
  screaming-eagle-cdn:latest
```

**With Environment Variables:**

```bash
docker run -d \
  --name cdn \
  -p 8080:8080 \
  -v $(pwd)/config:/app/config:ro \
  -e RUST_LOG=info \
  -e CDN_CONFIG_PATH=/app/config/cdn.toml \
  screaming-eagle-cdn:latest
```

#### Health Checks

Add Docker health checks for automatic restarts:

```bash
docker run -d \
  --name cdn \
  -p 8080:8080 \
  -v $(pwd)/config:/app/config:ro \
  --health-cmd="curl -f http://localhost:8080/_cdn/health || exit 1" \
  --health-interval=30s \
  --health-timeout=10s \
  --health-retries=3 \
  screaming-eagle-cdn:latest
```

### Docker Compose

For multi-container setups with monitoring.

#### Basic Setup

**docker-compose.yml:**

```yaml
version: '3.8'

services:
  cdn:
    image: screaming-eagle-cdn:latest
    build: .
    ports:
      - "8080:8080"
    volumes:
      - ./config:/app/config:ro
      - cdn-cache:/tmp/cdn-cache
    environment:
      - RUST_LOG=info
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/_cdn/health"]
      interval: 30s
      timeout: 10s
      retries: 3
    deploy:
      resources:
        limits:
          cpus: '2'
          memory: 2G
        reservations:
          cpus: '1'
          memory: 1G

volumes:
  cdn-cache:
```

#### With Monitoring Stack

**docker-compose.yml:**

```yaml
version: '3.8'

services:
  cdn:
    image: screaming-eagle-cdn:latest
    build: .
    ports:
      - "8080:8080"
    volumes:
      - ./config:/app/config:ro
    environment:
      - RUST_LOG=info
    restart: unless-stopped
    networks:
      - cdn-network

  prometheus:
    image: prom/prometheus:latest
    ports:
      - "9090:9090"
    volumes:
      - ./config/prometheus.yml:/etc/prometheus/prometheus.yml:ro
      - prometheus-data:/prometheus
    command:
      - '--config.file=/etc/prometheus/prometheus.yml'
      - '--storage.tsdb.path=/prometheus'
      - '--web.console.libraries=/usr/share/prometheus/console_libraries'
      - '--web.console.templates=/usr/share/prometheus/consoles'
    restart: unless-stopped
    networks:
      - cdn-network

  grafana:
    image: grafana/grafana:latest
    ports:
      - "3000:3000"
    volumes:
      - grafana-data:/var/lib/grafana
    environment:
      - GF_SECURITY_ADMIN_PASSWORD=admin
    restart: unless-stopped
    networks:
      - cdn-network

networks:
  cdn-network:
    driver: bridge

volumes:
  prometheus-data:
  grafana-data:
```

**Start the stack:**

```bash
docker-compose up -d
```

**Access:**
- CDN: http://localhost:8080
- Prometheus: http://localhost:9090
- Grafana: http://localhost:3000 (admin/admin)

### Kubernetes

Deploy to Kubernetes for orchestration and auto-scaling.

#### Namespace

```yaml
# namespace.yaml
apiVersion: v1
kind: Namespace
metadata:
  name: cdn
```

#### ConfigMap

```yaml
# configmap.yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: cdn-config
  namespace: cdn
data:
  cdn.toml: |
    [server]
    host = "0.0.0.0"
    port = 8080
    workers = 4
    request_timeout_seconds = 30

    [cache]
    max_size_bytes = 1073741824  # 1GB
    default_ttl_seconds = 3600

    [origins.example]
    url = "https://example.com"
    timeout_seconds = 5

    # ... rest of configuration
```

#### Deployment

```yaml
# deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: cdn
  namespace: cdn
  labels:
    app: cdn
spec:
  replicas: 3
  selector:
    matchLabels:
      app: cdn
  template:
    metadata:
      labels:
        app: cdn
    spec:
      containers:
      - name: cdn
        image: screaming-eagle-cdn:latest
        ports:
        - containerPort: 8080
          name: http
        volumeMounts:
        - name: config
          mountPath: /app/config
          readOnly: true
        env:
        - name: RUST_LOG
          value: "info"
        resources:
          requests:
            memory: "512Mi"
            cpu: "500m"
          limits:
            memory: "2Gi"
            cpu: "2000m"
        livenessProbe:
          httpGet:
            path: /_cdn/health
            port: 8080
          initialDelaySeconds: 10
          periodSeconds: 30
          timeoutSeconds: 5
        readinessProbe:
          httpGet:
            path: /_cdn/health
            port: 8080
          initialDelaySeconds: 5
          periodSeconds: 10
          timeoutSeconds: 5
      volumes:
      - name: config
        configMap:
          name: cdn-config
```

#### Service

```yaml
# service.yaml
apiVersion: v1
kind: Service
metadata:
  name: cdn-service
  namespace: cdn
spec:
  type: LoadBalancer
  selector:
    app: cdn
  ports:
  - protocol: TCP
    port: 80
    targetPort: 8080
    name: http
```

#### Horizontal Pod Autoscaler

```yaml
# hpa.yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: cdn-hpa
  namespace: cdn
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: cdn
  minReplicas: 3
  maxReplicas: 10
  metrics:
  - type: Resource
    resource:
      name: cpu
      target:
        type: Utilization
        averageUtilization: 70
  - type: Resource
    resource:
      name: memory
      target:
        type: Utilization
        averageUtilization: 80
```

#### Deploy to Kubernetes

```bash
kubectl apply -f namespace.yaml
kubectl apply -f configmap.yaml
kubectl apply -f deployment.yaml
kubectl apply -f service.yaml
kubectl apply -f hpa.yaml

# Check status
kubectl get pods -n cdn
kubectl get svc -n cdn
```

### Bare Metal

Deploy directly on a Linux server.

#### Build from Source

```bash
# Clone repository
git clone https://github.com/yourusername/Screaming-Eagle.git
cd Screaming-Eagle

# Build release binary
cargo build --release

# Binary location
ls -lh target/release/screaming-eagle-cdn
```

#### Install as Systemd Service

**1. Copy binary:**

```bash
sudo cp target/release/screaming-eagle-cdn /usr/local/bin/
sudo chmod +x /usr/local/bin/screaming-eagle-cdn
```

**2. Create configuration directory:**

```bash
sudo mkdir -p /etc/screaming-eagle
sudo cp config/cdn.toml /etc/screaming-eagle/
```

**3. Create systemd service:**

```bash
sudo tee /etc/systemd/system/screaming-eagle.service <<EOF
[Unit]
Description=Screaming Eagle CDN
After=network.target

[Service]
Type=simple
User=cdn
Group=cdn
WorkingDirectory=/var/lib/screaming-eagle
ExecStart=/usr/local/bin/screaming-eagle-cdn --config /etc/screaming-eagle/cdn.toml
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal
SyslogIdentifier=screaming-eagle

# Security
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/screaming-eagle

# Resource limits
LimitNOFILE=65535
MemoryLimit=4G

[Install]
WantedBy=multi-user.target
EOF
```

**4. Create user and directories:**

```bash
sudo useradd -r -s /bin/false cdn
sudo mkdir -p /var/lib/screaming-eagle
sudo chown -R cdn:cdn /var/lib/screaming-eagle
```

**5. Enable and start:**

```bash
sudo systemctl daemon-reload
sudo systemctl enable screaming-eagle
sudo systemctl start screaming-eagle
sudo systemctl status screaming-eagle
```

**6. View logs:**

```bash
sudo journalctl -u screaming-eagle -f
```

### Cloud Platforms

#### AWS

**Option 1: ECS (Elastic Container Service)**

1. Push image to ECR:
```bash
aws ecr create-repository --repository-name screaming-eagle-cdn
docker tag screaming-eagle-cdn:latest <account>.dkr.ecr.<region>.amazonaws.com/screaming-eagle-cdn:latest
docker push <account>.dkr.ecr.<region>.amazonaws.com/screaming-eagle-cdn:latest
```

2. Create ECS task definition with:
   - Container image from ECR
   - 8080 port mapping
   - Config via environment variables or S3-mounted config
   - CloudWatch logging

3. Create ECS service with Application Load Balancer

**Option 2: EKS (Kubernetes)**

Use Kubernetes deployment method above on EKS cluster.

#### Google Cloud Platform

**Option 1: Cloud Run**

```bash
# Build and push to Container Registry
gcloud builds submit --tag gcr.io/<project>/screaming-eagle-cdn

# Deploy to Cloud Run
gcloud run deploy screaming-eagle-cdn \
  --image gcr.io/<project>/screaming-eagle-cdn \
  --platform managed \
  --port 8080 \
  --memory 2Gi \
  --cpu 2 \
  --min-instances 1 \
  --max-instances 10
```

**Option 2: GKE (Kubernetes)**

Use Kubernetes deployment method on GKE cluster.

#### Azure

**Option 1: Container Instances**

```bash
az container create \
  --resource-group cdn-rg \
  --name screaming-eagle-cdn \
  --image screaming-eagle-cdn:latest \
  --ports 8080 \
  --cpu 2 \
  --memory 2 \
  --restart-policy always
```

**Option 2: AKS (Kubernetes)**

Use Kubernetes deployment method on AKS cluster.

#### DigitalOcean

**App Platform:**

1. Connect GitHub repository
2. Configure as Docker deployment
3. Set port to 8080
4. Add config as environment variables
5. Set instance size and scaling rules

## Configuration

### Environment-Specific Configs

Create separate config files for each environment:

```
config/
├── cdn.toml              # Default/development
├── cdn.staging.toml      # Staging environment
└── cdn.production.toml   # Production environment
```

**Load specific config:**

```bash
./screaming-eagle-cdn --config config/cdn.production.toml
```

### Configuration via Environment Variables

Override specific settings with environment variables:

```bash
export CDN_SERVER_HOST="0.0.0.0"
export CDN_SERVER_PORT="8080"
export CDN_CACHE_MAX_SIZE_BYTES="2147483648"
export CDN_ADMIN_TOKEN="secure-random-token"
```

### Secrets Management

**Never commit secrets to version control.**

**Docker Secrets:**

```bash
echo "secret-token" | docker secret create cdn_admin_token -
```

**Kubernetes Secrets:**

```bash
kubectl create secret generic cdn-secrets \
  --from-literal=admin-token=secret-token \
  -n cdn
```

Reference in deployment:

```yaml
env:
- name: CDN_ADMIN_TOKEN
  valueFrom:
    secretKeyRef:
      name: cdn-secrets
      key: admin-token
```

**AWS Secrets Manager:**

Use AWS SDK to fetch secrets at runtime.

## TLS/HTTPS Setup

### Generate Self-Signed Certificate (Development)

```bash
openssl req -x509 -newkey rsa:4096 \
  -keyout config/key.pem \
  -out config/cert.pem \
  -days 365 -nodes \
  -subj "/CN=localhost"
```

### Production Certificates

**Option 1: Let's Encrypt with Certbot**

```bash
sudo certbot certonly --standalone \
  -d cdn.example.com \
  --email admin@example.com
```

Certificates stored in `/etc/letsencrypt/live/cdn.example.com/`

**Option 2: Cloud Provider Certificates**

Use AWS ACM, GCP Managed Certificates, or Azure Key Vault.

### Configure TLS in cdn.toml

```toml
[tls]
enabled = true
cert_path = "/etc/letsencrypt/live/cdn.example.com/fullchain.pem"
key_path = "/etc/letsencrypt/live/cdn.example.com/privkey.pem"
```

### TLS Termination at Load Balancer

For cloud deployments, terminate TLS at the load balancer:

- AWS: Application Load Balancer with ACM certificate
- GCP: HTTPS Load Balancer with managed certificate
- Azure: Application Gateway with Key Vault certificate

CDN runs on HTTP internally, load balancer handles HTTPS.

## Monitoring Setup

### Prometheus

**1. Configure Prometheus to scrape CDN:**

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'screaming-eagle-cdn'
    static_configs:
      - targets: ['cdn:8080']
    metrics_path: '/_cdn/metrics'
    scrape_interval: 15s
```

**2. Start Prometheus:**

```bash
docker run -d \
  --name prometheus \
  -p 9090:9090 \
  -v $(pwd)/config/prometheus.yml:/etc/prometheus/prometheus.yml \
  prom/prometheus
```

### Grafana Dashboards

**1. Add Prometheus data source:**

- URL: http://prometheus:9090
- Access: Server (default)

**2. Import dashboard or create custom:**

Key metrics to monitor:
- Request rate (cdn_requests_total)
- Cache hit ratio (cdn_cache_hits_total / cdn_requests_total)
- Request duration (cdn_request_duration_seconds)
- Cache size (cdn_cache_size_bytes)
- Origin bytes transferred (cdn_origin_bytes_total)

**3. Example Grafana query:**

Cache hit ratio:
```promql
rate(cdn_cache_hits_total[5m]) / rate(cdn_requests_total[5m])
```

### Log Aggregation

**Option 1: ELK Stack (Elasticsearch, Logstash, Kibana)**

Configure JSON logging in cdn.toml:

```toml
[logging]
format = "json"
level = "info"
```

Ship logs to Elasticsearch via Filebeat or Logstash.

**Option 2: Cloud Logging**

- AWS: CloudWatch Logs
- GCP: Cloud Logging
- Azure: Application Insights

### Alerting

**Prometheus AlertManager rules:**

```yaml
groups:
  - name: cdn_alerts
    rules:
      - alert: HighErrorRate
        expr: rate(cdn_requests_total{status=~"5.."}[5m]) > 0.05
        for: 5m
        annotations:
          summary: "High error rate on CDN"

      - alert: LowCacheHitRatio
        expr: rate(cdn_cache_hits_total[15m]) / rate(cdn_requests_total[15m]) < 0.7
        for: 10m
        annotations:
          summary: "Cache hit ratio below 70%"

      - alert: CircuitBreakerOpen
        expr: cdn_circuit_breaker_state{state="open"} > 0
        for: 2m
        annotations:
          summary: "Circuit breaker open for {{ $labels.origin }}"
```

## High Availability

### Multi-Instance Deployment

Deploy multiple CDN instances behind a load balancer.

**Considerations:**

1. **Shared Nothing**: Each instance has independent cache
2. **Consistent Hashing**: Load balancer should use consistent hashing to maximize cache hits
3. **Health Checks**: Load balancer should check `/_cdn/health`
4. **Session Affinity**: Not required (CDN is stateless)

### Load Balancer Configuration

**Nginx:**

```nginx
upstream cdn_backend {
    least_conn;
    server cdn1:8080 max_fails=3 fail_timeout=30s;
    server cdn2:8080 max_fails=3 fail_timeout=30s;
    server cdn3:8080 max_fails=3 fail_timeout=30s;
}

server {
    listen 80;
    server_name cdn.example.com;

    location / {
        proxy_pass http://cdn_backend;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }

    location /_cdn/health {
        proxy_pass http://cdn_backend;
        access_log off;
    }
}
```

**HAProxy:**

```
frontend cdn_frontend
    bind *:80
    default_backend cdn_backend

backend cdn_backend
    balance leastconn
    option httpchk GET /_cdn/health
    server cdn1 cdn1:8080 check
    server cdn2 cdn2:8080 check
    server cdn3 cdn3:8080 check
```

### Geographic Distribution

Deploy CDN instances in multiple regions:

1. Deploy instances in different geographic regions
2. Use GeoDNS to route users to nearest instance
3. Configure origins appropriately for each region

### Disaster Recovery

**Backup Configuration:**

```bash
# Backup configuration
tar -czf cdn-config-backup-$(date +%Y%m%d).tar.gz config/

# Store in S3/GCS/Azure Blob
aws s3 cp cdn-config-backup-*.tar.gz s3://cdn-backups/
```

**Recovery Plan:**

1. Deploy new instances from Docker image
2. Restore configuration from backup
3. Update DNS/load balancer to point to new instances
4. Cache will rebuild automatically from origin

## Production Checklist

Before going to production, verify:

### Security

- [ ] TLS/HTTPS enabled with valid certificates
- [ ] Admin token changed from default
- [ ] IP allowlist configured for admin endpoints
- [ ] Security headers enabled
- [ ] Rate limiting configured
- [ ] Running as non-root user
- [ ] Secrets stored securely (not in version control)

### Performance

- [ ] Cache size appropriately configured
- [ ] Worker count matches CPU cores
- [ ] Connection pooling configured
- [ ] Compression enabled (gzip/brotli)
- [ ] Appropriate timeouts configured

### Reliability

- [ ] Circuit breaker enabled with proper thresholds
- [ ] Health checks configured
- [ ] stale-if-error configured for resilience
- [ ] Multiple instances deployed
- [ ] Load balancer health checks configured

### Monitoring

- [ ] Prometheus scraping configured
- [ ] Grafana dashboards created
- [ ] Alerts configured for critical metrics
- [ ] Logging aggregation set up
- [ ] Request tracing enabled

### Operations

- [ ] Deployment automation (CI/CD)
- [ ] Rollback procedure documented
- [ ] Backup strategy for configuration
- [ ] On-call rotation established
- [ ] Runbook created for common issues

### Testing

- [ ] Load testing completed
- [ ] Failover testing completed
- [ ] Cache purging tested
- [ ] Circuit breaker behavior verified
- [ ] Integration tests passing

## Scaling Strategies

### Vertical Scaling

Increase resources for existing instances:

**Docker:**
```bash
docker update --cpus="4" --memory="4g" cdn
```

**Kubernetes:**
```yaml
resources:
  limits:
    cpu: "4"
    memory: "4Gi"
```

### Horizontal Scaling

Add more instances:

**Kubernetes HPA:**
```bash
kubectl scale deployment cdn --replicas=6 -n cdn
```

**Docker Swarm:**
```bash
docker service scale cdn=6
```

### Cache Optimization

Improve cache efficiency:

1. **Increase cache size:**
   ```toml
   [cache]
   max_size_bytes = 10737418240  # 10GB
   ```

2. **Tune TTLs:**
   ```toml
   [origins.example]
   default_ttl_seconds = 7200  # 2 hours
   ```

3. **Enable stale-while-revalidate:**
   ```toml
   [cache]
   stale_while_revalidate_seconds = 300
   ```

### Multi-Tier Caching

Layer multiple caching tiers:

1. **Edge CDN**: Screaming Eagle instances at edge locations
2. **Regional Cache**: Screaming Eagle instances in regions
3. **Origin Shield**: Single Screaming Eagle instance before origin

Each tier caches content, reducing origin load.

### Database Caching (Future)

For distributed cache sharing, consider adding:

- Redis for shared cache backend
- Memcached for session storage
- Database for cache metadata

This is not currently implemented but could be added as an enhancement.

## Troubleshooting Deployment

### Container Won't Start

```bash
# Check logs
docker logs cdn

# Common issues:
# - Config file not mounted correctly
# - Port already in use
# - Insufficient memory
```

### High Memory Usage

```bash
# Check cache size
curl http://localhost:8080/_cdn/stats

# Reduce cache size in config
[cache]
max_size_bytes = 536870912  # 512MB
```

### Performance Issues

```bash
# Check metrics
curl http://localhost:8080/_cdn/metrics | grep duration

# Increase workers
[server]
workers = 8  # Match CPU cores
```

### Origin Connectivity

```bash
# Check circuit breaker status
curl -H "Authorization: Bearer token" \
  http://localhost:8080/_cdn/circuit-breakers

# Check origin health
curl -H "Authorization: Bearer token" \
  http://localhost:8080/_cdn/origins/health
```

## Support

For deployment issues:

- Check logs for error messages
- Verify configuration syntax
- Review the [Troubleshooting Guide](TROUBLESHOOTING.md)
- Open an issue on GitHub

## Next Steps

After deployment:

1. Configure monitoring and alerts
2. Set up log aggregation
3. Perform load testing
4. Document your specific deployment
5. Create runbooks for operations team
