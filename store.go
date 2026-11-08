package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"syscall"
	"time"
)

// The store: standard library only, no database. One JSON file whose first
// line names the format and the SHA-256 of the rest; a file whose checksum,
// JSON or rules do not hold is refused, never repaired. A change writes a
// new file beside the old one (0600), syncs it, renames it over the old one
// and syncs the folder, so the file is always one whole version. The
// service holds an exclusive lock on a companion .lock file while it runs,
// so two processes never share a store. Jobs live in memory, as before;
// the store keeps the download cache and the sign-in state.

const storeFormat = "katteke.ytdlp-web.store.v1"

type cacheEntry struct {
	CacheKey  string `json:"cache_key"`
	FilePath  string `json:"file_path"`
	Filename  string `json:"filename"`
	CreatedAt string `json:"created_at"`
}

type session struct {
	TokenHash  string `json:"token_hash"`
	AuthMethod string `json:"auth_method"`
	ExpiresAt  int64  `json:"expires_at"`
}

type pendingFlow struct {
	StateHash    string `json:"state_hash"`
	FlowHash     string `json:"flow_hash"`
	Nonce        string `json:"nonce"`
	PKCEVerifier string `json:"pkce_verifier"`
	ExpiresAt    int64  `json:"expires_at"`
}

type storeData struct {
	DownloadCache []cacheEntry  `json:"download_cache"`
	Sessions      []session     `json:"sessions"`
	PendingFlows  []pendingFlow `json:"pending_flows"`
}

type storeHeader struct {
	Format string `json:"format"`
	SHA256 string `json:"sha256"`
}

type store struct {
	path     string
	lockFile *os.File

	mu   sync.Mutex
	data *storeData
}

// storePath turns YTDLP_WEB_DB into the store's path: the same folder and
// name with .json, so the setting keeps its Rust value
// (data/ytdlp_web.sqlite becomes data/ytdlp_web.json).
func storePath(db string) string {
	base := strings.TrimSuffix(db, filepath.Ext(db))
	if base == "" || strings.HasSuffix(base, "/") {
		base = db
	}
	return base + ".json"
}

// openStore takes the lock and reads the store, creating an empty one when
// none is there.
func openStore(db string) (*store, error) {
	path := storePath(db)
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, err
	}
	s := &store{path: path}
	if err := s.lock(); err != nil {
		return nil, err
	}
	data, err := s.load()
	if errors.Is(err, os.ErrNotExist) {
		s.data = &storeData{}
		if err := s.update(func(*storeData) error { return nil }); err != nil {
			s.close()
			return nil, err
		}
		return s, nil
	}
	if err != nil {
		s.close()
		return nil, err
	}
	s.data = data
	return s, nil
}

// lock holds the companion lock file for the life of the process.
func (s *store) lock() error {
	path := s.path + ".lock"
	if info, err := os.Lstat(path); err == nil && !info.Mode().IsRegular() {
		return fmt.Errorf("store lock must be a regular file: %s", path)
	}
	file, err := os.OpenFile(path, os.O_RDWR|os.O_CREATE|syscall.O_NOFOLLOW, 0o600)
	if err != nil {
		return fmt.Errorf("failed to open store lock: %w", err)
	}
	if err := syscall.Flock(int(file.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		file.Close()
		return fmt.Errorf("the store %s is in use by another process", s.path)
	}
	s.lockFile = file
	return nil
}

func (s *store) close() {
	if s.lockFile != nil {
		syscall.Flock(int(s.lockFile.Fd()), syscall.LOCK_UN)
		s.lockFile.Close()
		s.lockFile = nil
	}
}

func (s *store) load() (*storeData, error) {
	info, err := os.Lstat(s.path)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, fmt.Errorf("store path must be a regular file: %s", s.path)
	}
	file, err := os.OpenFile(s.path, os.O_RDONLY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	raw, err := io.ReadAll(file)
	if err != nil {
		return nil, err
	}
	data, err := decodeStore(raw)
	if err != nil {
		return nil, fmt.Errorf("store %s is damaged and was not used: %w", s.path, err)
	}
	return data, nil
}

func decodeStore(raw []byte) (*storeData, error) {
	headerLine, body, found := bytes.Cut(raw, []byte("\n"))
	if !found {
		return nil, errors.New("no header line")
	}
	var header storeHeader
	if err := strictJSON(headerLine, &header); err != nil {
		return nil, fmt.Errorf("header: %w", err)
	}
	if header.Format != storeFormat {
		return nil, fmt.Errorf("unknown format %q", header.Format)
	}
	sum := sha256.Sum256(body)
	if hex.EncodeToString(sum[:]) != header.SHA256 {
		return nil, errors.New("checksum does not match its contents")
	}
	var data storeData
	if err := strictJSON(body, &data); err != nil {
		return nil, err
	}
	if err := data.validate(); err != nil {
		return nil, err
	}
	return &data, nil
}

func strictJSON(raw []byte, v any) error {
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(v); err != nil {
		return err
	}
	if decoder.More() {
		return errors.New("trailing data")
	}
	return nil
}

func encodeJSON(v any, indent bool) []byte {
	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	if indent {
		encoder.SetIndent("", "  ")
	}
	if err := encoder.Encode(v); err != nil {
		panic(err) // only plain structs are encoded
	}
	return buf.Bytes()
}

func encodeStore(data *storeData) []byte {
	body := encodeJSON(data, true)
	sum := sha256.Sum256(body)
	header := encodeJSON(storeHeader{storeFormat, hex.EncodeToString(sum[:])}, false)
	return append(header, body...)
}

// writeFileAtomic writes a new owner-only file beside path, syncs it,
// renames it over path and syncs the folder.
func writeFileAtomic(path string, content []byte) error {
	dir := filepath.Dir(path)
	fresh, err := os.CreateTemp(dir, "."+filepath.Base(path)+".new-*")
	if err != nil {
		return err
	}
	name := fresh.Name()
	_, err = fresh.Write(content)
	if err == nil {
		err = fresh.Chmod(0o600)
	}
	if err == nil {
		err = fresh.Sync()
	}
	if closeErr := fresh.Close(); err == nil {
		err = closeErr
	}
	if err == nil {
		err = os.Rename(name, path)
	}
	if err != nil {
		os.Remove(name)
		return err
	}
	if folder, err := os.Open(dir); err == nil {
		folder.Sync()
		folder.Close()
	}
	return nil
}

func (d *storeData) clone() *storeData {
	c := *d
	c.DownloadCache = slices.Clone(d.DownloadCache)
	c.Sessions = slices.Clone(d.Sessions)
	c.PendingFlows = slices.Clone(d.PendingFlows)
	return &c
}

// view runs fn on the current data; fn must not keep or change it.
func (s *store) view(fn func(*storeData)) {
	s.mu.Lock()
	defer s.mu.Unlock()
	fn(s.data)
}

// update runs fn on a copy and keeps the copy only when fn succeeds, every
// rule still holds and the file is written: a failed change leaves the
// store as it was, like a rolled-back transaction.
func (s *store) update(fn func(*storeData) error) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	next := s.data.clone()
	if err := fn(next); err != nil {
		return err
	}
	if err := next.validate(); err != nil {
		return err
	}
	if err := writeFileAtomic(s.path, encodeStore(next)); err != nil {
		return fmt.Errorf("failed to write store: %w", err)
	}
	s.data = next
	return nil
}

// validate is what the SQL schema enforced: the cache key and the session
// and flow hashes as primary keys, and the session method's CHECK.
func (d *storeData) validate() error {
	keys := map[string]bool{}
	for _, e := range d.DownloadCache {
		if keys[e.CacheKey] {
			return errors.New("UNIQUE constraint failed: download_cache.cache_key")
		}
		keys[e.CacheKey] = true
	}
	tokens := map[string]bool{}
	for _, s := range d.Sessions {
		if tokens[s.TokenHash] {
			return errors.New("UNIQUE constraint failed: auth_sessions.token_hash")
		}
		if s.AuthMethod != "local" && s.AuthMethod != "oidc" {
			return errors.New("CHECK constraint failed: auth_method IN ('local', 'oidc')")
		}
		tokens[s.TokenHash] = true
	}
	states := map[string]bool{}
	for _, f := range d.PendingFlows {
		if states[f.StateHash] {
			return errors.New("UNIQUE constraint failed: oidc_pending_flows.state_hash")
		}
		states[f.StateHash] = true
	}
	return nil
}

// rfc3339 is chrono's to_rfc3339(): fractional seconds in 0, 3, 6 or 9
// digits and a +00:00 offset.
func rfc3339(t time.Time) string {
	t = t.UTC()
	return t.Format("2006-01-02T15:04:05") + fraction(t.Nanosecond()) + "+00:00"
}

// jsonTime is how chrono serialized a DateTime<Utc> into JSON: the same
// fraction and a Z.
func jsonTime(t time.Time) string {
	t = t.UTC()
	return t.Format("2006-01-02T15:04:05") + fraction(t.Nanosecond()) + "Z"
}

func fraction(nanos int) string {
	switch {
	case nanos == 0:
		return ""
	case nanos%1_000_000 == 0:
		return fmt.Sprintf(".%03d", nanos/1_000_000)
	case nanos%1_000 == 0:
		return fmt.Sprintf(".%06d", nanos/1_000)
	default:
		return fmt.Sprintf(".%09d", nanos)
	}
}

// asciiLower folds A-Z only.
func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if 'A' <= c && c <= 'Z' {
			b[i] = c + 32
		}
	}
	return string(b)
}
