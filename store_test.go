package main

import (
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// The Rust migration tests become store tests.

func TestNewStoreIsEmptyAndOwnerOnly(t *testing.T) {
	dir := t.TempDir()
	st, err := openStore(filepath.Join(dir, "ytdlp_web.sqlite"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.close()
	if len(st.data.DownloadCache) != 0 || len(st.data.Sessions) != 0 {
		t.Fatal(st.data)
	}
	if info, err := os.Stat(filepath.Join(dir, "ytdlp_web.json")); err != nil || info.Mode().Perm() != 0o600 {
		t.Fatal(err, info)
	}
}

func TestDamagedStoreIsRefusedNotRepaired(t *testing.T) {
	db := filepath.Join(t.TempDir(), "ytdlp_web.sqlite")
	st, _ := openStore(db)
	st.update(func(d *storeData) error {
		d.DownloadCache = append(d.DownloadCache, cacheEntry{CacheKey: "youtube:a", FilePath: "/x/a.mp4", Filename: "a.mp4", CreatedAt: "now"})
		return nil
	})
	st.close()
	raw, _ := os.ReadFile(storePath(db))
	os.WriteFile(storePath(db), bytes.Replace(raw, []byte("a.mp4"), []byte("b.mp4"), 1), 0o600)
	if _, err := openStore(db); err == nil || !strings.Contains(err.Error(), "checksum") {
		t.Fatal(err)
	}
}

func TestTheStoreIsUsedByOneProcessAtATime(t *testing.T) {
	db := filepath.Join(t.TempDir(), "ytdlp_web.sqlite")
	st, err := openStore(db)
	if err != nil {
		t.Fatal(err)
	}
	defer st.close()
	if _, err := openStore(db); err == nil || !strings.Contains(err.Error(), "in use") {
		t.Fatal(err)
	}
}

func TestAFailedChangeLeavesNothingBehind(t *testing.T) {
	st, _ := openStore(filepath.Join(t.TempDir(), "ytdlp_web.sqlite"))
	defer st.close()
	before, _ := os.ReadFile(st.path)
	err := st.update(func(d *storeData) error {
		d.Sessions = append(d.Sessions, session{TokenHash: "a", AuthMethod: "password", ExpiresAt: 1})
		return nil
	})
	if err == nil || !strings.Contains(err.Error(), "CHECK") {
		t.Fatal(err)
	}
	err = st.update(func(d *storeData) error {
		d.DownloadCache = append(d.DownloadCache, cacheEntry{CacheKey: "k"}, cacheEntry{CacheKey: "k"})
		return nil
	})
	if err == nil || !strings.Contains(err.Error(), "UNIQUE") {
		t.Fatal(err)
	}
	after, _ := os.ReadFile(st.path)
	if !bytes.Equal(before, after) || len(st.data.Sessions) != 0 {
		t.Fatal("a failed change was kept")
	}
}

func TestAnUnimportedRustDatabaseStopsTheStart(t *testing.T) {
	db := filepath.Join(t.TempDir(), "ytdlp_web.sqlite")
	os.WriteFile(db, []byte("SQLite format 3\x00"), 0o600)
	if _, err := openStore(db); err == nil || !strings.Contains(err.Error(), "import-sqlite") {
		t.Fatal(err)
	}
}

func TestImportFromASchema2Database(t *testing.T) {
	if _, err := exec.LookPath("sqlite3"); err != nil {
		t.Skip("sqlite3 is not installed")
	}
	dir := t.TempDir()
	source := filepath.Join(dir, "rust.sqlite")
	cmd := exec.Command("sqlite3", source)
	cmd.Stdin = strings.NewReader(`
CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT (datetime('now')));
INSERT INTO schema_migrations (version) VALUES (1), (2);
CREATE TABLE download_cache (cache_key TEXT PRIMARY KEY, file_path TEXT NOT NULL, filename TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE auth_sessions (token_hash TEXT PRIMARY KEY, auth_method TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE oidc_pending_flows (state_hash TEXT PRIMARY KEY, flow_hash TEXT NOT NULL, nonce TEXT NOT NULL, pkce_verifier TEXT NOT NULL, expires_at INTEGER NOT NULL);
INSERT INTO download_cache VALUES ('youtube:abc', '/srv/dl/1/A [abc].mp4', 'A [abc].mp4', '2026-11-01T10:00:00.123+00:00');
INSERT INTO auth_sessions VALUES ('h', 'local', 9999999999);
`)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatal(string(out))
	}
	db := filepath.Join(dir, "go", "ytdlp_web.sqlite")
	counts, err := importSQLite(db, source)
	if err != nil || counts.DownloadCache != 1 || counts.Dropped.Sessions != 1 {
		t.Fatal(err, counts)
	}
	if _, err := importSQLite(db, source); err == nil || !strings.Contains(err.Error(), "refused to overwrite") {
		t.Fatal(err)
	}
	st, err := openStore(db)
	if err != nil {
		t.Fatal(err)
	}
	defer st.close()
	want := cacheEntry{CacheKey: "youtube:abc", FilePath: "/srv/dl/1/A [abc].mp4", Filename: "A [abc].mp4", CreatedAt: "2026-11-01T10:00:00.123+00:00"}
	if len(st.data.DownloadCache) != 1 || st.data.DownloadCache[0] != want || len(st.data.Sessions) != 0 {
		t.Fatal(st.data)
	}
}
