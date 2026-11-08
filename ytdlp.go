package main

import (
	"bufio"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"
	"unicode"
	"unicode/utf8"
)

// The yt-dlp process: one run per job in the job's folder, stdout read line
// by line for progress, stderr drained alongside (only its tail kept for
// the error), and the process killed when the wall-clock limit passes.

// stderrTailBytes is how much of the end of stderr the error message keeps.
const stderrTailBytes = 4 * 1024

const formatChoice = "bv*[ext=mp4][vcodec^=avc1][height<=1080]+ba[ext=m4a]/bv*[ext=mp4][height<=1080]+ba[ext=m4a]/b[ext=mp4][vcodec^=avc1][height<=720]/b[ext=mp4][height<=720]/b[ext=mp4]"

func download(program, jsRuntime, jobDir, link string, timeout time.Duration, onLine func(line, progress string)) (string, error) {
	args := []string{"--no-playlist", "--newline", "--no-part", "--restrict-filenames", "--windows-filenames", "--no-mtime",
		"--socket-timeout", "30", "--retries", "3", "--fragment-retries", "3", "-f", formatChoice, "--merge-output-format", "mp4"}
	if jsRuntime != "" {
		args = append(args, "--js-runtimes", "node:"+jsRuntime)
	}
	args = append(args, "-o", filepath.Join(jobDir, "%(title).180B [%(id)s].%(ext)s"), link)
	cmd := exec.Command(program, args...)
	cmd.Dir = jobDir
	if err := run(cmd, timeout, onLine); err != nil {
		return "", err
	}
	file, ok := findDownloadedFile(jobDir)
	if !ok {
		return "", errNoFile
	}
	return file, nil
}

// run starts the command: stdout lines go to onLine, stderr is drained
// concurrently (only its tail is kept), and the process is killed once the
// timeout passes.
func run(cmd *exec.Cmd, timeout time.Duration, onLine func(line, progress string)) error {
	cmd.Stdin = nil
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("Could not start yt-dlp: %s", rustIOError(err))
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return fmt.Errorf("Could not start yt-dlp: %s", rustIOError(err))
	}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("Could not start yt-dlp: %s", rustIOError(err))
	}
	tail := make(chan string, 1)
	go func() { tail <- drainTail(stderr) }()
	done := make(chan struct{})
	go func() {
		eachLine(stdout, func(line string) { onLine(line, progressFromLine(line)) })
		close(done)
	}()
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case <-done:
	case <-timer.C:
		cmd.Process.Kill()
		go cmd.Wait()
		return fmt.Errorf("yt-dlp was stopped after %d seconds without finishing.", int64(timeout/time.Second))
	}
	// Wait for the exit within what is left of the limit.
	exited := make(chan error, 1)
	go func() { exited <- cmd.Wait() }()
	var waitErr error
	select {
	case waitErr = <-exited:
	case <-timer.C:
		cmd.Process.Kill()
		return fmt.Errorf("yt-dlp was stopped after %d seconds without finishing.", int64(timeout/time.Second))
	}
	text := strings.TrimSpace(<-tail)
	if waitErr != nil {
		var exitErr *exec.ExitError
		if !errors.As(waitErr, &exitErr) {
			return fmt.Errorf("yt-dlp wait failed: %s", rustIOError(waitErr))
		}
		code := exitErr.ExitCode()
		if text == "" {
			return fmt.Errorf("yt-dlp exited with code %d", code)
		}
		return fmt.Errorf("yt-dlp exited with code %d: %s", code, text)
	}
	return nil
}

// drainTail reads stderr to the end, keeping its last stderrTailBytes, cut
// on a character boundary; each line is trimmed at its end.
func drainTail(stderr io.Reader) string {
	kept := ""
	eachLine(stderr, func(line string) {
		kept += strings.TrimRightFunc(line, unicode.IsSpace) + "\n"
		if len(kept) > stderrTailBytes {
			cut := len(kept) - stderrTailBytes
			for cut < len(kept) && !utf8.RuneStart(kept[cut]) {
				cut++
			}
			kept = kept[cut:]
		}
	})
	return kept
}

// eachLine is BufReader::lines(): lines of any length, without the "\n" or
// "\r\n" that ends them.
func eachLine(r io.Reader, fn func(string)) {
	reader := bufio.NewReader(r)
	for {
		line, err := reader.ReadString('\n')
		if line != "" {
			line = strings.TrimSuffix(line, "\n")
			fn(strings.TrimSuffix(line, "\r"))
		}
		if err != nil {
			return
		}
	}
}

// progressFromLine finds "[download]  42.7% ..." and returns "42.7%".
func progressFromLine(line string) string {
	index := strings.Index(line, "[download]")
	if index < 0 {
		return ""
	}
	rest := line[index+len("[download]"):]
	percent := strings.IndexByte(rest, '%')
	if percent < 0 {
		return ""
	}
	fields := strings.Fields(rest[:percent])
	if len(fields) == 0 {
		return ""
	}
	number := fields[len(fields)-1]
	for _, c := range number {
		if !(c >= '0' && c <= '9' || c == '.') {
			return ""
		}
	}
	return number + "%"
}

// rustIOError words an operating-system error as Rust's io::Error did:
// "No such file or directory (os error 2)".
func rustIOError(err error) string {
	if errors.Is(err, exec.ErrNotFound) {
		err = syscall.ENOENT
	}
	var errno syscall.Errno
	if errors.As(err, &errno) {
		text := errno.Error()
		if text != "" {
			text = strings.ToUpper(text[:1]) + text[1:]
		}
		return fmt.Sprintf("%s (os error %d)", text, int(errno))
	}
	return err.Error()
}
