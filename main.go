package main

import (
	"bufio"
	"database/sql"
	"flag"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"charm.land/bubbles/v2/textinput"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/dustin/go-humanize"
	"github.com/sahilm/fuzzy"
	_ "modernc.org/sqlite"
)

var (
	styleCursor lipgloss.Style
	styleDim    lipgloss.Style
)

func init() {
	styleCursor = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("4"))
	styleDim = lipgloss.NewStyle().Faint(true)
}

func dbDir() string {
	dir := os.Getenv("XDG_DATA_HOME")
	if dir == "" {
		dir = filepath.Join(os.Getenv("HOME"), ".local", "share")
	}

	dir = filepath.Join(dir, "stinkpot")

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
		);

		-- walk rows newest-first and stop at the limit instead of scanning
		-- the whole table and sorting it on every invocation.
		create index if not exists history_ts_cmd on history(ts desc, cmd);`); err != nil {
		_ = db.Close()
		return nil, err
	}

	return db, nil
}

type candidate struct {
	cmd string
	ts  time.Time
}

func loadCandidates(db *sql.DB) ([]candidate, error) {
	rows, err := db.Query(`
		select cmd, ts from history
		order by ts desc
		limit 10000`,
	)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()

	var out []candidate
	for rows.Next() {
		var c string
		var ts int64
		if err := rows.Scan(&c, &ts); err == nil {
			out = append(out, candidate{cmd: c, ts: time.Unix(ts, 0)})
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
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		return
	}
	defer func() { _ = db.Close() }()

	// re-run of the same command bumps its timestamp
	cwd, _ := os.Getwd()
	if _, err := db.Exec(
		`insert into history(cmd, cwd, exit, ts, session) values(?, ?, ?, ?, ?)
		 on conflict(cmd) do update set
		   cwd     = excluded.cwd,
		   exit    = excluded.exit,
		   ts      = excluded.ts,
		   session = excluded.session`,
		cmd, cwd, *exit, time.Now().Unix(), os.Getenv("stinkpot_SESSION"),
	); err != nil {
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
	}
}

func search(args []string) {
	fs := flag.NewFlagSet("search", flag.ExitOnError)
	_ = fs.Parse(args)
	initial := strings.Join(fs.Args(), " ")

	db, err := open(dbPath())
	if err != nil {
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}
	cands, err := loadCandidates(db)
	_ = db.Close()
	if err != nil {
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}

	ti := textinput.New()
	ti.Prompt = "> "
	ti.Placeholder = "search history..."
	styles := textinput.DefaultStyles(true)
	styles.Focused.Placeholder = styleDim
	styles.Blurred.Placeholder = styleDim
	ti.SetStyles(styles)
	ti.SetValue(initial)
	ti.CursorEnd()
	ti.Focus()

	m := &model{textinput: ti, all: cands}
	m.filter()

	p := tea.NewProgram(m, tea.WithOutput(os.Stderr), tea.WithInput(os.Stdin))
	res, err := p.Run()
	if err != nil {
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}
	if fm, ok := res.(*model); ok && fm.selected != "" {
		fmt.Println(fm.selected)
	}
}

func list(_ []string) {
	db, err := open(dbPath())
	if err != nil {
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}
	defer func() { _ = db.Close() }()

	rows, err := db.Query(`select ts, exit, cmd from history order by id desc limit 50`)
	if err != nil {
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}
	defer func() { _ = rows.Close() }()
	for rows.Next() {
		if rows.Err() != nil {
			fmt.Fprintln(os.Stderr, "stinkpot:", err)
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
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}
	defer func() { _ = f.Close() }()

	db, err := open(dbPath())
	if err != nil {
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}
	defer func() { _ = db.Close() }()

	tx, err := db.Begin()
	if err != nil {
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}

	stmt, err := tx.Prepare(`
		insert into history(cmd, cwd, exit, ts, session) values(?, ?, ?, ?, ?)
		on conflict(cmd) do update set ts = max(ts, excluded.ts)`)
	if err != nil {
		_ = tx.Rollback()
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}
	defer func() { _ = stmt.Close() }()

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
			fmt.Fprintln(os.Stderr, "stinkpot:", err)
			os.Exit(1)
		}

		last = cmd
		n++
	}

	if err := sc.Err(); err != nil {
		_ = tx.Rollback()
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}

	if err := tx.Commit(); err != nil {
		fmt.Fprintln(os.Stderr, "stinkpot:", err)
		os.Exit(1)
	}

	fmt.Fprintf(os.Stderr, "stinkpot: imported %d commands from %s\n", n, path)
}

// ui
const maxRows = 12

type model struct {
	textinput textinput.Model
	all       []candidate
	filtered  []candidate
	cursor    int
	selected  string
}

type candidateSource []candidate

func (c candidateSource) String(i int) string { return c[i].cmd }
func (c candidateSource) Len() int            { return len(c) }

func (m *model) filter() {
	q := m.textinput.Value()

	if strings.TrimSpace(q) == "" {
		m.filtered = m.all
	} else {
		matches := fuzzy.FindFrom(q, candidateSource(m.all))
		out := make([]candidate, len(matches))
		for i, mt := range matches {
			out[i] = m.all[mt.Index]
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

// shortRelTime renders t as a compact "time ago" string (e.g. "3min", "2d").
func shortRelTime(t time.Time) string {
	if t.Unix() == 0 {
		return "never"
	}
	return humanize.CustomRelTime(t, time.Now(), "", "", []humanize.RelTimeMagnitude{
		{D: time.Second, Format: "now", DivBy: time.Second},
		{D: 2 * time.Second, Format: "1s", DivBy: 1},
		{D: time.Minute, Format: "%ds", DivBy: time.Second},
		{D: 2 * time.Minute, Format: "1m", DivBy: 1},
		{D: time.Hour, Format: "%dm", DivBy: time.Minute},
		{D: 2 * time.Hour, Format: "1hr", DivBy: 1},
		{D: humanize.Day, Format: "%dhrs", DivBy: time.Hour},
		{D: 2 * humanize.Day, Format: "1d", DivBy: 1},
		{D: 20 * humanize.Day, Format: "%dd", DivBy: humanize.Day},
		{D: 8 * humanize.Week, Format: "%dw", DivBy: humanize.Week},
		{D: humanize.Year, Format: "%dmo", DivBy: humanize.Month},
		{D: 18 * humanize.Month, Format: "1y", DivBy: 1},
		{D: 2 * humanize.Year, Format: "2y", DivBy: 1},
		{D: humanize.LongTime, Format: "%dy", DivBy: humanize.Year},
		{D: math.MaxInt64, Format: "a long while ago", DivBy: 1},
	})
}

func (m *model) Init() tea.Cmd { return textinput.Blink }

func (m *model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	if key, ok := msg.(tea.KeyPressMsg); ok {
		switch key.String() {
		case "ctrl+c", "esc":
			m.selected = ""
			return m, tea.Quit
		case "enter", "tab":
			if m.cursor >= 0 && m.cursor < len(m.filtered) {
				m.selected = m.filtered[m.cursor].cmd
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

func (m *model) View() tea.View {
	var b strings.Builder
	b.WriteString(m.textinput.View())
	b.WriteByte('\n')

	start := 0
	if m.cursor >= maxRows {
		start = m.cursor - maxRows + 1
	}
	end := min(start+maxRows, len(m.filtered))

	// width of the widest relative time in view, so commands line up.
	tsWidth := 0
	for i := start; i < end; i++ {
		if w := lipgloss.Width(shortRelTime(m.filtered[i].ts)); w > tsWidth {
			tsWidth = w
		}
	}

	for i := start; i < end; i++ {
		c := m.filtered[i]
		line := strings.ReplaceAll(c.cmd, "\n", "  ")
		ts := styleDim.Render(fmt.Sprintf("%*s", tsWidth, shortRelTime(c.ts)))
		if i == m.cursor {
			b.WriteString(ts)
			b.WriteByte(' ')
			b.WriteString(styleCursor.Render(line))
		} else {
			b.WriteString(ts)
			b.WriteByte(' ')
			b.WriteString(line)
		}
		b.WriteByte('\n')
	}

	b.WriteString(styleDim.Render(fmt.Sprintf(
		"  %d matches · ↑/↓ move · enter accept · esc cancel", len(m.filtered))))
	v := tea.NewView(b.String())
	v.AltScreen = true
	return v
}

// bash side of things
const initScript = `# run eval "$(stinkpot init)" at startup
__stinkpot_record() {
  local exit=$?
  local cmd
  cmd=$(HISTTIMEFORMAT='' history 1 | sed '1 s/^[[:space:]]*[0-9]\{1,\}[[:space:]]*//')
  [ -n "$cmd" ] && stinkpot add --exit "$exit" -- "$cmd"
}
case "$PROMPT_COMMAND" in
  *__stinkpot_record*) ;;
  *) PROMPT_COMMAND="__stinkpot_record${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
esac

__stinkpot_search() {
  local out
  out=$(stinkpot search -- "$READLINE_LINE") || return
  if [ -n "$out" ]; then
    READLINE_LINE="$out"
    READLINE_POINT=${#READLINE_LINE}
  fi
}
bind -x '"\C-r": __stinkpot_search'
`

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: stinkpot <init|add|search|list|import>")
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
		fmt.Fprintf(os.Stderr, "stinkpot: unknown command %q\n", os.Args[1])
		os.Exit(1)
	}
}
