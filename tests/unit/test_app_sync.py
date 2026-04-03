"""Unit tests for app_sync.py Flask blueprint

Tests cover the /api/sync bulk-sync endpoint:
- Full sync (no since parameter)
- Incremental sync (since parameter)
- Pagination (page, limit, has_more, next_page)
- Embedding toggle (include_embeddings)
- Field mappings (author -> artist, raw energy)
- UMAP coordinates
- Provider mappings
- Error handling
"""
import json
import sys
import base64
import pytest
import numpy as np
from datetime import datetime, timezone
from unittest.mock import Mock, MagicMock, patch, call

flask = pytest.importorskip('flask', reason='Flask not installed')
from flask import Flask


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

class FakeDictRow(dict):
    """Mimics psycopg2 DictRow with both dict-key and attribute access."""
    def __getattr__(self, name):
        try:
            return self[name]
        except KeyError:
            raise AttributeError(name)


def _make_track_row(track_id=1, title="Test Song", author="Test Artist",
                    album="Test Album", energy=0.08, tempo=120.0,
                    updated_at=None):
    """Create a fake score+track joined row."""
    if updated_at is None:
        updated_at = datetime(2026, 1, 15, 12, 0, 0, tzinfo=timezone.utc)
    return FakeDictRow({
        'track_id': track_id,
        'file_path_hash': f'hash{track_id:060d}'[:64],
        'title': title,
        'author': author,
        'album_artist': author,
        'album': album,
        'duration_seconds': 240,
        'year': 2020,
        'track_number': 1,
        'disc_number': 1,
        'tempo': tempo,
        'key': 'C Major',
        'scale': 'major',
        'mood_vector': 'rock:0.85,pop:0.40',
        'energy': energy,
        'other_features': 'danceable:0.7,happy:0.6',
        'rating': 4,
        'updated_at': updated_at,
    })


def _make_combined_cursor(count, rows):
    """Create a cursor mock used for both COUNT(*) fetchone and main fetchall."""
    cur = MagicMock()
    cur.fetchone.return_value = [count]
    cur.fetchall.return_value = rows
    return cur


def _make_simple_cursor(rows=None):
    """Create a cursor mock for a single fetchall query."""
    cur = MagicMock()
    cur.fetchall.return_value = rows or []
    return cur


def _setup_db_cursors(mock_db, count=0, rows=None, emb_rows=None,
                      clap_rows=None, prov_rows=None, deleted_rows=None,
                      include_embeddings=True, has_since=False):
    """Configure mock_db to return the right cursors in the right order.

    Cursor order in app_sync.py:
    1. DictCursor: COUNT + main SELECT (fetchone then fetchall)
    2. cursor(): embedding SELECT (only if include_embeddings and track_ids)
    3. cursor(): clap_embedding SELECT (only if include_embeddings and track_ids)
    4. DictCursor: provider_track SELECT (only if track_ids)
    5. cursor(): deleted_tracks SELECT (only if has_since)
    """
    rows = rows or []
    has_tracks = len(rows) > 0
    cursor_list = [_make_combined_cursor(count, rows)]

    if include_embeddings and has_tracks:
        cursor_list.append(_make_simple_cursor(emb_rows or []))
        cursor_list.append(_make_simple_cursor(clap_rows or []))

    if has_tracks:
        cursor_list.append(_make_simple_cursor(prov_rows or []))

    if has_since:
        cursor_list.append(_make_simple_cursor(deleted_rows or []))

    it = iter(cursor_list)
    mock_db.cursor.side_effect = lambda **kw: next(it)
    return cursor_list


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def mock_db():
    """Create a mock database connection."""
    return MagicMock()


@pytest.fixture
def app(mock_db):
    """Create a Flask app with the sync blueprint registered."""
    with patch('app_helper.get_db', return_value=mock_db), \
         patch('app_helper.load_map_projection', return_value=(None, None)):
        from app_sync import sync_bp
        flask_app = Flask(__name__)
        flask_app.register_blueprint(sync_bp)
        flask_app.config['TESTING'] = True
        yield flask_app


@pytest.fixture
def client(app):
    return app.test_client()


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestSyncDefaults:
    """GET /api/sync with no parameters."""

    def test_returns_200_with_correct_structure(self, client, mock_db):
        _setup_db_cursors(mock_db)
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        assert resp.status_code == 200
        data = resp.get_json()
        for key in ('tracks', 'deleted_ids', 'total_tracks', 'model_version', 'has_more', 'next_page'):
            assert key in data

    def test_empty_database(self, client, mock_db):
        _setup_db_cursors(mock_db)
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        data = resp.get_json()
        assert data['tracks'] == []
        assert data['deleted_ids'] == []
        assert data['total_tracks'] == 0
        assert data['has_more'] is False
        assert data['next_page'] is None

    def test_model_version_present(self, client, mock_db):
        _setup_db_cursors(mock_db)
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        assert resp.get_json()['model_version'] == 'musicnn_v0.9.3'


class TestFieldMappings:
    """Verify field names and values in track objects."""

    def test_author_mapped_to_artist(self, client, mock_db):
        row = _make_track_row(author="Pink Floyd")
        _setup_db_cursors(mock_db, count=1, rows=[row])
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        track = resp.get_json()['tracks'][0]
        assert track['artist'] == 'Pink Floyd'
        assert 'author' not in track

    def test_energy_not_normalized(self, client, mock_db):
        row = _make_track_row(energy=0.08)
        _setup_db_cursors(mock_db, count=1, rows=[row])
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        track = resp.get_json()['tracks'][0]
        assert track['energy'] == 0.08

    def test_null_fields_handled(self, client, mock_db):
        row = FakeDictRow({
            'track_id': 1, 'file_path_hash': 'a' * 64,
            'title': 'Test', 'author': 'Artist', 'album_artist': None,
            'album': None, 'duration_seconds': None, 'year': None,
            'track_number': None, 'disc_number': None, 'tempo': None,
            'key': None, 'scale': None, 'mood_vector': None,
            'energy': None, 'other_features': None, 'rating': None,
            'updated_at': None,
        })
        _setup_db_cursors(mock_db, count=1, rows=[row])
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        assert resp.status_code == 200
        track = resp.get_json()['tracks'][0]
        assert track['year'] is None
        assert track['rating'] is None
        assert track['tempo'] is None
        assert track['energy'] is None
        assert track['umap_x'] is None
        assert track['umap_y'] is None

    def test_updated_at_iso_format(self, client, mock_db):
        dt = datetime(2026, 3, 15, 14, 30, 0, tzinfo=timezone.utc)
        row = _make_track_row(updated_at=dt)
        _setup_db_cursors(mock_db, count=1, rows=[row])
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        track = resp.get_json()['tracks'][0]
        assert '2026-03-15' in track['updated_at']


class TestEmbeddings:
    """Embedding inclusion and base64 encoding."""

    def test_embeddings_included_by_default(self, client, mock_db):
        row = _make_track_row(track_id=1)
        emb_bytes = np.random.randn(200).astype(np.float32).tobytes()
        _setup_db_cursors(mock_db, count=1, rows=[row],
                          emb_rows=[(1, emb_bytes)])
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        track = resp.get_json()['tracks'][0]
        assert 'embedding' in track
        assert track['embedding'] is not None

    def test_embedding_base64_roundtrip(self, client, mock_db):
        row = _make_track_row(track_id=1)
        original = np.array([1.0, 2.0, 3.0], dtype=np.float32)
        _setup_db_cursors(mock_db, count=1, rows=[row],
                          emb_rows=[(1, original.tobytes())])
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        track = resp.get_json()['tracks'][0]
        decoded = np.frombuffer(base64.b64decode(track['embedding']), dtype=np.float32)
        np.testing.assert_array_almost_equal(decoded, original)

    def test_include_embeddings_false(self, client, mock_db):
        row = _make_track_row(track_id=1)
        _setup_db_cursors(mock_db, count=1, rows=[row], include_embeddings=False)
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync?include_embeddings=false')
        track = resp.get_json()['tracks'][0]
        assert 'embedding' not in track
        assert 'clap_embedding' not in track


class TestPagination:
    """Page, limit, has_more, next_page behavior."""

    def test_has_more_true(self, client, mock_db):
        rows = [_make_track_row(i) for i in range(5)]
        _setup_db_cursors(mock_db, count=10, rows=rows)
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync?page=1&limit=5')
        data = resp.get_json()
        assert data['has_more'] is True
        assert data['next_page'] == 2

    def test_has_more_false_on_last_page(self, client, mock_db):
        rows = [_make_track_row(i) for i in range(5)]
        _setup_db_cursors(mock_db, count=5, rows=rows)
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync?page=1&limit=5')
        data = resp.get_json()
        assert data['has_more'] is False
        assert data['next_page'] is None

    def test_limit_capped_at_1000(self, client, mock_db):
        cursors = _setup_db_cursors(mock_db, count=0)
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync?limit=5000')
        assert resp.status_code == 200
        # The combined cursor's second execute call (main query) should have limit=1000
        combined_cur = cursors[0]
        main_call = combined_cur.execute.call_args_list[1]
        # params list is [limit, offset] — limit should be clamped to 1000
        assert main_call[0][1][-2] == 1000


class TestIncrementalSync:
    """Incremental sync with `since` parameter."""

    def test_invalid_since_returns_400(self, client, mock_db):
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync?since=not-a-date')
        assert resp.status_code == 400
        assert 'error' in resp.get_json()

    def test_since_returns_deleted_ids(self, client, mock_db):
        _setup_db_cursors(mock_db, count=0, has_since=True,
                          deleted_rows=[(456,), (789,)])
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync?since=2025-01-01T00:00:00Z')
        data = resp.get_json()
        assert 456 in data['deleted_ids']
        assert 789 in data['deleted_ids']

    def test_full_sync_has_empty_deleted_ids(self, client, mock_db):
        _setup_db_cursors(mock_db)
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        assert resp.get_json()['deleted_ids'] == []


class TestUmapCoordinates:
    """UMAP coordinate inclusion from map_projection_data."""

    def test_umap_coords_included(self, client, mock_db):
        row = _make_track_row(track_id=42)
        _setup_db_cursors(mock_db, count=1, rows=[row])

        id_map = [42, 99]
        proj = np.array([[12.34, -5.67], [1.0, 2.0]], dtype=np.float32)

        with patch('config.APP_VERSION', 'v0.9.3'), \
             patch('app_helper.load_map_projection', return_value=(id_map, proj)):
            resp = client.get('/api/sync')
        track = resp.get_json()['tracks'][0]
        assert abs(track['umap_x'] - 12.34) < 0.01
        assert abs(track['umap_y'] - (-5.67)) < 0.01

    def test_umap_null_when_no_projections(self, client, mock_db):
        row = _make_track_row()
        _setup_db_cursors(mock_db, count=1, rows=[row])

        with patch('config.APP_VERSION', 'v0.9.3'), \
             patch('app_helper.load_map_projection', return_value=(None, None)):
            resp = client.get('/api/sync')
        track = resp.get_json()['tracks'][0]
        assert track['umap_x'] is None
        assert track['umap_y'] is None


class TestProviderMappings:
    """Provider mapping inclusion per track."""

    def test_provider_mappings_included(self, client, mock_db):
        row = _make_track_row(track_id=1)
        prov_rows = [
            FakeDictRow({'track_id': 1, 'provider_id': 1, 'provider_type': 'navidrome', 'item_id': 'abc-123'}),
            FakeDictRow({'track_id': 1, 'provider_id': 2, 'provider_type': 'jellyfin', 'item_id': 'jf-456'}),
        ]
        _setup_db_cursors(mock_db, count=1, rows=[row], prov_rows=prov_rows)
        with patch('config.APP_VERSION', 'v0.9.3'):
            resp = client.get('/api/sync')
        track = resp.get_json()['tracks'][0]
        assert len(track['provider_mappings']) == 2
        assert track['provider_mappings'][0]['provider_type'] == 'navidrome'
        assert track['provider_mappings'][0]['item_id'] == 'abc-123'
        assert track['provider_mappings'][1]['provider_type'] == 'jellyfin'
