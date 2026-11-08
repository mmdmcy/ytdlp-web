package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// import-sqlite: the one-time move from the Rust service's SQLite database.
// The download cache comes over as it was; sessions and pending sign-ins
// are short-lived and stay behind: people sign in again. Jobs were never
// stored. The database is read with the system's sqlite3 command, read-only
// and immutable (meant for a copy, never the live file). Only counts are
// printed.

const sqliteSchemaVersion = 2

type sqliteReader struct{ uri string }

func newSQLiteReader(path string) (*sqliteReader, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, fmt.Errorf("failed to inspect %s: %w", path, err)
	}
	if !info.Mode().IsRegular() {
		return nil, fmt.Errorf("%s must be a regular file (a copy of the database)", path)
	}
	if _, err := exec.LookPath("sqlite3"); err != nil {
		return nil, errors.New("the sqlite3 command is missing; install it (apt install sqlite3)")
	}
	escaped := (&url.URL{Path: path}).EscapedPath()
	return &sqliteReader{uri: "file:" + escaped + "?mode=ro&immutable=1"}, nil
}

// rows runs one query and decodes its JSON rows into out.
func (r *sqliteReader) rows(query string, out any) error {
	cmd := exec.Command("sqlite3", "-readonly", "-json", r.uri, query)
	cmd.Env = append(os.Environ(), "SQLITE_HISTORY=/dev/null")
	var stderr strings.Builder
	cmd.Stderr = &stderr
	output, err := cmd.Output()
	if err != nil {
		return fmt.Errorf("sqlite3 failed: %s", strings.TrimSpace(stderr.String()))
	}
	text := strings.TrimSpace(string(output))
	if text == "" {
		text = "[]"
	}
	return json.Unmarshal([]byte(text), out)
}

func importSQLiteCmd(args []string) error {
	flags := flag.NewFlagSet("import-sqlite", flag.ContinueOnError)
	from := flags.String("from", "", "a copy of the Rust service's database")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if *from == "" || flags.NArg() > 0 {
		return errors.New("usage: ytdlp-web import-sqlite --from COPY (the store goes where YTDLP_WEB_DB points, as .json)")
	}
	counts, err := importSQLite(envOr("YTDLP_WEB_DB", "data/ytdlp_web.sqlite"), *from)
	if err != nil {
		return err
	}
	os.Stdout.Write(encodeJSON(counts, false))
	return nil
}

type importCounts struct {
	Store         string `json:"store"`
	DownloadCache int    `json:"download_cache"`
	Dropped       struct {
		Sessions     int `json:"sessions"`
		PendingFlows int `json:"pending_flows"`
	} `json:"dropped"`
}

func importSQLite(db, from string) (*importCounts, error) {
	path := storePath(db)
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, err
	}
	s := &store{path: path}
	if err := s.lock(); err != nil {
		return nil, err
	}
	defer s.close()
	if _, err := os.Lstat(path); err == nil {
		return nil, fmt.Errorf("refused to overwrite the store at %s", path)
	}
	reader, err := newSQLiteReader(from)
	if err != nil {
		return nil, err
	}
	var version []struct {
		Version *int `json:"version"`
	}
	if err := reader.rows("SELECT MAX(version) AS version FROM schema_migrations", &version); err != nil {
		return nil, err
	}
	if len(version) != 1 || version[0].Version == nil || *version[0].Version != sqliteSchemaVersion {
		return nil, fmt.Errorf("%s is not a schema-%d YTDLP Web database", from, sqliteSchemaVersion)
	}
	data := &storeData{}
	if err := reader.rows("SELECT cache_key, file_path, filename, created_at FROM download_cache ORDER BY rowid", &data.DownloadCache); err != nil {
		return nil, fmt.Errorf("import from %s failed: %w", from, err)
	}
	counts := &importCounts{Store: path, DownloadCache: len(data.DownloadCache)}
	var dropped []struct {
		Sessions int `json:"sessions"`
		Flows    int `json:"flows"`
	}
	if err := reader.rows("SELECT (SELECT COUNT(*) FROM auth_sessions) AS sessions, (SELECT COUNT(*) FROM oidc_pending_flows) AS flows", &dropped); err != nil {
		return nil, err
	}
	if len(dropped) == 1 {
		counts.Dropped.Sessions, counts.Dropped.PendingFlows = dropped[0].Sessions, dropped[0].Flows
	}
	if err := data.validate(); err != nil {
		return nil, fmt.Errorf("import from %s failed: %w", from, err)
	}
	if err := writeFileAtomic(path, encodeStore(data)); err != nil {
		return nil, err
	}
	return counts, nil
}
