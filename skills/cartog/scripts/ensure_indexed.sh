#!/usr/bin/env bash
set -euo pipefail

# Ensure the cartog index exists and is up to date.
# Run this at the start of a coding session.
#
# Three phases:
#   1. Code graph index (blocking, fast — incremental, < 1s for unchanged codebases)
#   2. Model download (blocking, one-time — enables cross-encoder reranker on FTS5 results)
#   3. RAG embedding (background — vector search becomes available when done)
#
# After phase 2, `cartog rag search` already works (FTS5 + reranker).
# Phase 3 adds vector/semantic matching in the background without blocking the agent.

DB_FILE=".cartog.db"

# Phase 1: Code graph index (always fast, incremental)
if [ ! -f "$DB_FILE" ]; then
    echo "No cartog index found. Building..."
else
    echo "Updating cartog index..."
fi
cartog index .

# Phase 2: Download embedding + reranker models (one-time, cached in ~/.cache/cartog/models/)
# This enables the cross-encoder reranker even before vector embeddings exist.
if ! cartog rag setup 2>/dev/null; then
    echo "Warning: cartog rag setup failed. Semantic search will use FTS5-only (no reranker)."
fi

# Phase 3: RAG embedding in background (non-blocking)
# Vector search becomes available once this completes.
RAG_LOG="/tmp/cartog-rag-index-$$.log"
nohup cartog rag index . > "$RAG_LOG" 2>&1 &
RAG_PID=$!
echo "RAG embedding started in background (PID $RAG_PID, log: $RAG_LOG)"
echo "cartog rag search works now (FTS5 + reranker). Vector search available when embedding completes."
