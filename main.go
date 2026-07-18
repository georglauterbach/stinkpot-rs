package main

import (
	"bufio"
	"database/sql"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/charmbracelet/bubbles/textinput"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/sahilm/fuzzy"
	_ "modernc.org/sqlite"
)

var (
	styleCursor lipgloss.Style
	styleDim    lipgloss.Style
)

func init() {
	lipgloss.SetDefaultRenderer(lipgloss.NewRenderer(os.Stderr))
	styleCursor = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("4"))
	styleDim = lipgloss.NewStyle().Faint(true)
}

func dbDir() string {
	dir := os.Getenv("XDG_DATA_HOME")
	if dir == "" {
		dir = filepath.Join(os.Getenv("HOME"), ".local", "share")
	}

	dir = filepath.Join(dir, "tortu")

	return dir
}

func dbPath() string {
	return filepath.Join(dbDir(), "history.db")
}

func open(path string) (*sql.DB, error) {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, err
	}

	dsn := "file:" + path +
		"?_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)"

	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, err
	}

	if _, err := db.Exec(`
		create table if not exists history (
		  id      integer primary key autoincrement,
		  cmd     text not null unique,
		  cwd     text,
		  exit    integer,
		  ts      integer,
		  session text
		);`); err != nil {
		db.Close()
		return nil, err
	}

	return db, nil
}

func loadCandidates(db *sql.DB) ([]string, error) {
	rows, err := db.Query(`
		select cmd from history
		order by ts desc
		limit 10000`,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var out []string
	for rows.Next() {
		var c string
		if err := rows.Scan(&c); err == nil {
			out = append(out, c)
		}
	}
	return out, rows.Err()
}

// cmds: store, search, list

func add(args []string) {
	fs := flag.NewFlagSet("add", flag.ExitOnError)
	exit := fs.Int("exit", 0, "exit code of the command")
	cmdFlag := fs.String("cmd", "", "command to record (alternative to positional args)")
	_ = fs.Parse(args)

	cmd := *cmdFlag
	if cmd == "" {
		cmd = strings.Join(fs.Args(), " ")
	}
	cmd = strings.TrimRight(cmd, " \t\n")
	if strings.TrimSpace(cmd) == "" {
		return
	}

	db, err := open(dbPath())
	if err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		return
	}
	defer db.Close()

	// re-run of the same command bumps its timestamp
	cwd, _ := os.Getwd()
	if _, err := db.Exec(
		`insert into history(cmd, cwd, exit, ts, session) values(?, ?, ?, ?, ?)
		 on conflict(cmd) do update set
		   cwd     = excluded.cwd,
		   exit    = excluded.exit,
		   ts      = excluded.ts,
		   session = excluded.session`,
		cmd, cwd, *exit, time.Now().Unix(), os.Getenv("TORTU_SESSION"),
	); err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
	}
}

func search(args []string) {
	fs := flag.NewFlagSet("search", flag.ExitOnError)
	_ = fs.Parse(args)
	initial := strings.Join(fs.Args(), " ")

	db, err := open(dbPath())
	if err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}
	cands, err := loadCandidates(db)
	db.Close()
	if err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}

	textinput := textinput.New()
	textinput.Prompt = "> "
	textinput.Placeholder = "search history..."
	textinput.PlaceholderStyle = styleDim
	textinput.SetValue(initial)
	textinput.CursorEnd()
	textinput.Focus()

	m := &model{textinput: textinput, all: cands}
	m.filter()

	p := tea.NewProgram(m, tea.WithOutput(os.Stderr), tea.WithInput(os.Stdin), tea.WithAltScreen())
	res, err := p.Run()
	if err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}
	if fm, ok := res.(*model); ok && fm.selected != "" {
		fmt.Println(fm.selected)
	}
}

func list(_ []string) {
	db, err := open(dbPath())
	if err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}
	defer db.Close()

	rows, err := db.Query(`select ts, exit, cmd from history order by id desc limit 50`)
	if err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}
	defer rows.Close()
	for rows.Next() {
		if rows.Err() != nil {
			fmt.Fprintln(os.Stderr, "tortu:", err)
			os.Exit(1)
		}
		var ts int64
		var exit int
		var cmd string
		if err := rows.Scan(&ts, &exit, &cmd); err == nil {
			fmt.Printf("%s\t%d\t%s\n", time.Unix(ts, 0).Format("2006-01-02 15:04"), exit, cmd)
		}
	}
}

func importBash(args []string) {
	fs := flag.NewFlagSet("import", flag.ExitOnError)
	file := fs.String("file", "", "history file to read (default: $HISTFILE, else ~/.bash_history)")
	_ = fs.Parse(args)

	path := *file
	if path == "" {
		path = os.Getenv("HISTFILE")
	}
	if path == "" {
		path = filepath.Join(os.Getenv("HOME"), ".bash_history")
	}

	f, err := os.Open(path)
	if err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}
	defer f.Close()

	db, err := open(dbPath())
	if err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}
	defer db.Close()

	tx, err := db.Begin()
	if err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}

	stmt, err := tx.Prepare(`
		insert into history(cmd, cwd, exit, ts, session) values(?, ?, ?, ?, ?)
		on conflict(cmd) do update set ts = max(ts, excluded.ts)`)
	if err != nil {
		_ = tx.Rollback()
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}
	defer stmt.Close()

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 1<<20) // tolerate long command lines

	ts := time.Now().Unix()
	var last string
	var n int
	for sc.Scan() {
		line := sc.Text()
		// with HISTTIMEFORMAT set, bash writes a "#<epoch>" line before each cmd.
		if after, ok := strings.CutPrefix(line, "#"); ok {
			if t, err := strconv.ParseInt(after, 10, 64); err == nil {
				ts = t
				continue
			}
		}

		cmd := strings.TrimRight(line, " \t")
		if strings.TrimSpace(cmd) == "" || cmd == last {
			continue
		}

		if _, err := stmt.Exec(cmd, "", 0, ts, ""); err != nil {
			_ = tx.Rollback()
			fmt.Fprintln(os.Stderr, "tortu:", err)
			os.Exit(1)
		}

		last = cmd
		n++
	}

	if err := sc.Err(); err != nil {
		_ = tx.Rollback()
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}

	if err := tx.Commit(); err != nil {
		fmt.Fprintln(os.Stderr, "tortu:", err)
		os.Exit(1)
	}

	fmt.Fprintf(os.Stderr, "tortu: imported %d commands from %s\n", n, path)
}

// ui
const maxRows = 12

type model struct {
	textinput textinput.Model
	all       []string
	filtered  []string
	cursor    int
	selected  string
}

func (m *model) filter() {
	q := m.textinput.Value()

	if strings.TrimSpace(q) == "" {
		m.filtered = m.all
	} else {
		matches := fuzzy.Find(q, m.all)
		out := make([]string, len(matches))
		for i, mt := range matches {
			out[i] = mt.Str
		}
		m.filtered = out
	}

	if m.cursor >= len(m.filtered) {
		m.cursor = len(m.filtered) - 1
	}

	if m.cursor < 0 {
		m.cursor = 0
	}
}

func (m *model) Init() tea.Cmd { return textinput.Blink }

func (m *model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	if key, ok := msg.(tea.KeyMsg); ok {
		switch key.String() {
		case "ctrl+c", "esc":
			m.selected = ""
			return m, tea.Quit
		case "enter", "tab":
			if m.cursor >= 0 && m.cursor < len(m.filtered) {
				m.selected = m.filtered[m.cursor]
			}
			return m, tea.Quit
		case "up", "ctrl+p":
			if m.cursor > 0 {
				m.cursor--
			}
			return m, nil
		case "down", "ctrl+n":
			if m.cursor < len(m.filtered)-1 {
				m.cursor++
			}
			return m, nil
		}
	}

	var cmd tea.Cmd
	m.textinput, cmd = m.textinput.Update(msg)
	m.filter()
	return m, cmd
}

func (m *model) View() string {
	var b strings.Builder
	b.WriteString(m.textinput.View())
	b.WriteByte('\n')

	start := 0
	if m.cursor >= maxRows {
		start = m.cursor - maxRows + 1
	}
	end := min(start+maxRows, len(m.filtered))
	for i := start; i < end; i++ {
		line := strings.ReplaceAll(m.filtered[i], "\n", "  ")
		if i == m.cursor {
			b.WriteString(styleCursor.Render("  " + line))
		} else {
			b.WriteString("  ")
			b.WriteString(line)
		}
		b.WriteByte('\n')
	}

	b.WriteString(styleDim.Render(fmt.Sprintf(
		"  %d matches · ↑/↓ move · enter accept · esc cancel", len(m.filtered))))
	return b.String()
}

// bash side of things
const initScript = `# run eval "$(tortu init)" at startup
__tortu_record() {
  local exit=$?
  local cmd
  cmd=$(HISTTIMEFORMAT='' history 1 | sed '1 s/^[[:space:]]*[0-9]\{1,\}[[:space:]]*//')
  [ -n "$cmd" ] && tortu add --exit "$exit" -- "$cmd"
}
case "$PROMPT_COMMAND" in
  *__tortu_record*) ;;
  *) PROMPT_COMMAND="__tortu_record${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
esac

__tortu_search() {
  local out
  out=$(tortu search -- "$READLINE_LINE") || return
  if [ -n "$out" ]; then
    READLINE_LINE="$out"
    READLINE_POINT=${#READLINE_LINE}
  fi
}
bind -x '"\C-r": __tortu_search'
`

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: tortu <init|add|search|list|import>")
		os.Exit(1)
	}

	switch os.Args[1] {
	case "init":
		fmt.Print(initScript)
	case "add":
		add(os.Args[2:])
	case "search":
		search(os.Args[2:])
	case "list":
		list(os.Args[2:])
	case "import":
		importBash(os.Args[2:])
	default:
		fmt.Fprintf(os.Stderr, "tortu: unknown command %q\n", os.Args[1])
		os.Exit(1)
	}
}
