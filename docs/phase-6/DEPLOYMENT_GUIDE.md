# Phase 6 Deployment Guide

## Prerequisites
- Docker + Compose
- `.env` in `deploy/compose/` with required secrets (see `.env.example`)

## Start
```bash
cd deploy/compose
cp ../../.env.example .env  # edit values
docker compose up -d
```

## Services
- postgres, nats, control-plane, scheduler, worker-agent, obscura-supervisor

## Health
- control-plane: http://localhost:8081/
- scheduler: http://localhost:8082/
- obscura CDP: http://127.0.0.1:9222/json/version
