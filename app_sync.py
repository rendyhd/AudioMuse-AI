# app_sync.py - Bulk sync endpoint for Auralscape mobile app

import base64
import logging
from datetime import datetime

import config
from flask import Blueprint, jsonify, request
from psycopg2.extras import DictCursor

# NOTE: get_db and load_map_projection are imported inside each function to prevent circular imports.

logger = logging.getLogger(__name__)

sync_bp = Blueprint('sync_bp', __name__)


@sync_bp.route('/api/sync', methods=['GET'])
def sync_tracks():
    """
    Bulk-sync endpoint for the Auralscape mobile app.
    Returns paginated track metadata, analysis features, embeddings, UMAP projections, and provider mappings.
    ---
    tags:
      - Sync
    parameters:
      - name: since
        in: query
        description: ISO 8601 timestamp for incremental sync. If omitted, returns full dataset.
        schema:
          type: string
      - name: include_embeddings
        in: query
        description: If false, omits embedding and clap_embedding fields.
        schema:
          type: boolean
          default: true
      - name: page
        in: query
        description: Pagination page number (1-indexed).
        schema:
          type: integer
          default: 1
      - name: limit
        in: query
        description: Tracks per page (max 1000).
        schema:
          type: integer
          default: 500
    responses:
      200:
        description: Paginated sync data.
      400:
        description: Invalid parameters.
      500:
        description: Internal server error.
    """
    from app_helper import get_db, load_map_projection

    # --- Parse parameters ---
    since = request.args.get('since')
    include_embeddings = request.args.get('include_embeddings', 'true').lower() != 'false'
    page = max(1, request.args.get('page', 1, type=int))
    limit = max(1, min(request.args.get('limit', 500, type=int), 1000))
    offset = (page - 1) * limit

    since_ts = None
    if since:
        try:
            since_ts = datetime.fromisoformat(since.replace('Z', '+00:00'))
        except ValueError:
            return jsonify({"error": "Invalid 'since' parameter. Use ISO 8601 format."}), 400

    db = get_db()
    try:
        cur = db.cursor(cursor_factory=DictCursor)

        # --- 1. Count total + fetch paginated tracks ---
        where_clause = ""
        params = []
        if since_ts:
            where_clause = "WHERE s.updated_at > %s"
            params = [since_ts]

        cur.execute(
            f"SELECT COUNT(*) FROM score s {where_clause}",
            params
        )
        total_tracks = cur.fetchone()[0]

        cur.execute(f"""
            SELECT s.track_id, t.file_path_hash, s.title, s.author, s.album_artist,
                   s.album, s.duration_seconds, s.year, s.track_number, s.disc_number,
                   s.tempo, s.key, s.scale, s.mood_vector, s.energy,
                   s.other_features, s.rating, s.updated_at
            FROM score s
            JOIN track t ON s.track_id = t.id
            {where_clause}
            ORDER BY s.track_id
            LIMIT %s OFFSET %s
        """, params + [limit, offset])
        rows = cur.fetchall()
        cur.close()

        track_ids = [row['track_id'] for row in rows]

        # --- 2. Look up UMAP coordinates for this page's tracks ---
        umap_coords = {}
        if track_ids:
            try:
                id_map, proj = load_map_projection('main_map')
                if id_map and proj is not None:
                    page_tids = set(track_ids)
                    for idx, tid in enumerate(id_map):
                        int_tid = int(tid)
                        if int_tid in page_tids and idx < len(proj):
                            umap_coords[int_tid] = (float(proj[idx][0]), float(proj[idx][1]))
            except Exception as e:
                logger.warning(f"Failed to load UMAP projections: {e}")

        # --- 3. Batch-fetch embeddings (conditional) ---
        emb_map = {}
        clap_emb_map = {}
        if include_embeddings and track_ids:
            cur = db.cursor()
            cur.execute("SELECT track_id, embedding FROM embedding WHERE track_id IN %s", (tuple(track_ids),))
            for row in cur.fetchall():
                emb_map[row[0]] = base64.b64encode(row[1]).decode('ascii') if row[1] else None
            cur.close()

            cur = db.cursor()
            cur.execute("SELECT track_id, embedding FROM clap_embedding WHERE track_id IN %s", (tuple(track_ids),))
            for row in cur.fetchall():
                clap_emb_map[row[0]] = base64.b64encode(row[1]).decode('ascii') if row[1] else None
            cur.close()

        # --- 4. Batch-fetch provider mappings ---
        provider_map = {}
        if track_ids:
            cur = db.cursor(cursor_factory=DictCursor)
            cur.execute("""
                SELECT pt.track_id, pt.provider_id, p.provider_type, pt.item_id
                FROM provider_track pt
                JOIN provider p ON pt.provider_id = p.id
                WHERE pt.track_id IN %s
            """, (tuple(track_ids),))
            for row in cur.fetchall():
                tid = row['track_id']
                if tid not in provider_map:
                    provider_map[tid] = []
                provider_map[tid].append({
                    "provider_id": row['provider_id'],
                    "provider_type": row['provider_type'],
                    "item_id": row['item_id']
                })
            cur.close()

        # --- 5. Fetch deleted track IDs (incremental sync only) ---
        deleted_ids = []
        if since_ts:
            cur = db.cursor()
            cur.execute("SELECT track_id FROM deleted_tracks WHERE deleted_at > %s", (since_ts,))
            deleted_ids = [row[0] for row in cur.fetchall()]
            cur.close()

        # --- 6. Assemble response ---
        has_more = (offset + limit) < total_tracks
        tracks_out = []
        for row in rows:
            tid = row['track_id']
            coords = umap_coords.get(tid, (None, None))
            track_data = {
                "id": tid,
                "file_path_hash": row['file_path_hash'],
                "title": row['title'],
                "artist": row['author'],
                "album_artist": row['album_artist'],
                "album": row['album'],
                "duration_seconds": row['duration_seconds'],
                "year": row['year'],
                "track_number": row['track_number'],
                "disc_number": row['disc_number'],
                "tempo": float(row['tempo']) if row['tempo'] is not None else None,
                "key": row['key'],
                "scale": row['scale'],
                "mood_vector": row['mood_vector'],
                "energy": float(row['energy']) if row['energy'] is not None else None,
                "other_features": row['other_features'],
                "rating": row['rating'],
                "umap_x": coords[0],
                "umap_y": coords[1],
                "provider_mappings": provider_map.get(tid, []),
                "updated_at": row['updated_at'].isoformat() if row['updated_at'] else None,
            }

            if include_embeddings:
                track_data["embedding"] = emb_map.get(tid)
                track_data["clap_embedding"] = clap_emb_map.get(tid)

            tracks_out.append(track_data)

        return jsonify({
            "tracks": tracks_out,
            "deleted_ids": deleted_ids,
            "total_tracks": total_tracks,
            "model_version": f"musicnn_{config.APP_VERSION}",
            "has_more": has_more,
            "next_page": page + 1 if has_more else None,
        })

    except Exception as e:
        logger.error(f"Error in sync endpoint: {e}", exc_info=True)
        return jsonify({"error": "Internal server error"}), 500
