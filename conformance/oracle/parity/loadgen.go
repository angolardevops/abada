package main

import (
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"math/rand"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

type mixRequest struct {
	Name     string            `json:"name"`
	Weight   int               `json:"weight"`
	Method   string            `json:"method"`
	Path     string            `json:"path"`
	Headers  map[string]string `json:"headers"`
	Body     string            `json:"body"`
	Expect   int               `json:"expect"`
	Excluded string            `json:"excluded"`
}

type mixFile struct {
	Requests []mixRequest `json:"requests"`
}

func loadMix(path, only string) []mixRequest {
	raw, err := os.ReadFile(path)
	must(err)
	var m mixFile
	must(json.Unmarshal(raw, &m))
	if only == "" {
		return m.Requests
	}
	for _, r := range m.Requests {
		if r.Name == only {
			r.Weight = 1
			return []mixRequest{r}
		}
	}
	must(fmt.Errorf("no request %q in the mix", only))
	return nil
}

func newClient(conns int) *http.Client {
	return &http.Client{Transport: &http.Transport{
		MaxIdleConns:        conns,
		MaxIdleConnsPerHost: conns,
		MaxConnsPerHost:     conns,
		IdleConnTimeout:     time.Minute,
		DisableCompression:  true,
	}}
}

func build(base string, r mixRequest) *http.Request {
	var body io.Reader
	if r.Body != "" {
		body = strings.NewReader(r.Body)
	}
	req, err := http.NewRequest(r.Method, base+r.Path, body)
	must(err)
	for k, v := range r.Headers {
		req.Header.Set(k, v)
	}
	return req
}

type sample struct {
	kind int
	ns   int64
	ok   bool
}

type result struct {
	Name   string  `json:"name"`
	Count  int     `json:"count"`
	Errors int     `json:"errors_unexpected_status"`
	RPS    float64 `json:"rps"`
	P50    float64 `json:"p50_us"`
	P90    float64 `json:"p90_us"`
	P99    float64 `json:"p99_us"`
	P999   float64 `json:"p999_us"`
	Max    float64 `json:"max_us"`
	Mean   float64 `json:"mean_us"`
}

func pct(sorted []int64, p float64) float64 {
	if len(sorted) == 0 {
		return 0
	}
	i := int(float64(len(sorted)-1) * p)
	return float64(sorted[i]) / 1e3 // µs
}

func summarize(name string, ns []int64, bad int, dur time.Duration) result {
	sort.Slice(ns, func(i, j int) bool { return ns[i] < ns[j] })
	var sum int64
	for _, v := range ns {
		sum += v
	}
	r := result{Name: name, Count: len(ns), Errors: bad, RPS: float64(len(ns)) / dur.Seconds(),
		P50: pct(ns, .5), P90: pct(ns, .9), P99: pct(ns, .99), P999: pct(ns, .999)}
	if len(ns) > 0 {
		r.Max = float64(ns[len(ns)-1]) / 1e3
		r.Mean = float64(sum) / float64(len(ns)) / 1e3
	}
	return r
}

// procStat reads utime+stime (clock ticks) and VmRSS (kB) of a process; the
// gateway is a separate process, so its CPU and memory are what a request costs.
func procStat(pid int) (ticks uint64, rssKB uint64) {
	if pid <= 0 {
		return
	}
	b, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if err == nil {
		s := string(b)
		f := strings.Fields(s[strings.LastIndexByte(s, ')')+2:])
		u, _ := strconv.ParseUint(f[11], 10, 64)
		k, _ := strconv.ParseUint(f[12], 10, 64)
		ticks = u + k
	}
	if st, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/status"); err == nil {
		for _, line := range strings.Split(string(st), "\n") {
			if strings.HasPrefix(line, "VmRSS:") {
				f := strings.Fields(line)
				rssKB, _ = strconv.ParseUint(f[1], 10, 64)
			}
		}
	}
	return
}

func runLoadgen(args []string) {
	fs := flag.NewFlagSet("loadgen", flag.ExitOnError)
	base := fs.String("url", "http://127.0.0.1:8080", "gateway base URL")
	mixPath := fs.String("mix", "benches/parity/mix.json", "request mix")
	only := fs.String("only", "", "one request of the mix instead of the weighted mix")
	conc := fs.Int("c", 8, "closed loop: concurrent connections; open loop: max in flight")
	rate := fs.Float64("rate", 0, "open loop: requests per second (0 = closed loop)")
	dur := fs.Duration("d", 30*time.Second, "measured duration")
	warm := fs.Duration("warmup", 10*time.Second, "warm-up before measuring")
	pid := fs.Int("pid", 0, "gateway pid: CPU and RSS are sampled from /proc")
	rssEvery := fs.Duration("rss-every", 0, "sample RSS this often and print the series (soak)")
	seed := fs.Int64("seed", 1, "request choice seed")
	_ = fs.Parse(args)

	mix := loadMix(*mixPath, *only)
	total := 0
	for _, r := range mix {
		total += r.Weight
	}
	pick := func(rng *rand.Rand) int {
		n := rng.Intn(total)
		for i, r := range mix {
			if n < r.Weight {
				return i
			}
			n -= r.Weight
		}
		return 0
	}
	client := newClient(*conc)
	var measuring atomic.Bool
	var mu sync.Mutex
	samples := make([]sample, 0, 1<<20)

	do := func(kind int, intended time.Time) {
		req := build(*base, mix[kind])
		resp, err := client.Do(req)
		ok := false
		if err == nil {
			_, _ = io.Copy(io.Discard, resp.Body)
			resp.Body.Close()
			ok = resp.StatusCode == mix[kind].Expect
		}
		if measuring.Load() {
			// From the INTENDED start: in an open loop a stalled server makes
			// every queued request wait, and that wait is latency.
			s := sample{kind: kind, ns: time.Since(intended).Nanoseconds(), ok: ok}
			mu.Lock()
			samples = append(samples, s)
			mu.Unlock()
		}
	}

	stop := make(chan struct{})
	var wg sync.WaitGroup
	if *rate <= 0 {
		for w := 0; w < *conc; w++ {
			wg.Add(1)
			go func(w int) {
				defer wg.Done()
				rng := rand.New(rand.NewSource(*seed + int64(w)))
				for {
					select {
					case <-stop:
						return
					default:
					}
					do(pick(rng), time.Now())
				}
			}(w)
		}
	} else {
		sem := make(chan struct{}, *conc)
		wg.Add(1)
		go func() {
			defer wg.Done()
			rng := rand.New(rand.NewSource(*seed))
			interval := time.Duration(float64(time.Second) / *rate)
			next := time.Now()
			var inner sync.WaitGroup
			defer inner.Wait()
			for {
				select {
				case <-stop:
					return
				default:
				}
				if d := time.Until(next); d > 0 {
					time.Sleep(d)
				}
				intended := next
				next = next.Add(interval)
				kind := pick(rng)
				sem <- struct{}{}
				inner.Add(1)
				go func() {
					defer inner.Done()
					defer func() { <-sem }()
					do(kind, intended)
				}()
			}
		}()
	}

	time.Sleep(*warm)
	t0 := time.Now()
	ticks0, _ := procStat(*pid)
	measuring.Store(true)
	var series []uint64
	if *rssEvery > 0 {
		for time.Since(t0) < *dur {
			time.Sleep(*rssEvery)
			_, rss := procStat(*pid)
			series = append(series, rss)
		}
	} else {
		time.Sleep(*dur)
	}
	measuring.Store(false)
	elapsed := time.Since(t0)
	ticks1, rss1 := procStat(*pid)
	close(stop)
	wg.Wait()

	mu.Lock()
	defer mu.Unlock()
	perKind := make([][]int64, len(mix))
	bad := make([]int, len(mix))
	var all []int64
	badAll := 0
	for _, s := range samples {
		if !s.ok {
			bad[s.kind]++
			badAll++
		}
		perKind[s.kind] = append(perKind[s.kind], s.ns)
		all = append(all, s.ns)
	}
	out := struct {
		Mode        string   `json:"mode"`
		Concurrency int      `json:"concurrency"`
		Rate        float64  `json:"target_rate,omitempty"`
		Duration    float64  `json:"seconds"`
		Overall     result   `json:"overall"`
		PerRequest  []result `json:"per_request,omitempty"`
		CPUMsPerReq float64  `json:"gateway_cpu_ms_per_request,omitempty"`
		RSSEndKB    uint64   `json:"gateway_rss_end_kb,omitempty"`
		RSSSeriesKB []uint64 `json:"gateway_rss_series_kb,omitempty"`
	}{Mode: "closed", Concurrency: *conc, Rate: *rate, Duration: elapsed.Seconds(), RSSEndKB: rss1, RSSSeriesKB: series}
	if *rate > 0 {
		out.Mode = "open"
	}
	out.Overall = summarize("mix", all, badAll, elapsed)
	if *only == "" {
		for i, r := range mix {
			if r.Weight > 0 {
				out.PerRequest = append(out.PerRequest, summarize(r.Name, perKind[i], bad[i], elapsed))
			}
		}
	}
	if *pid > 0 && len(all) > 0 {
		// 100 clock ticks per second on Linux (CLK_TCK); checked by getconf in the script.
		out.CPUMsPerReq = float64(ticks1-ticks0) * 10 / float64(len(all))
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	must(enc.Encode(out))
}

// runCheck prints status and body for every request of the mix, so two
// gateways can be compared before they are measured.
func runCheck(args []string) {
	fs := flag.NewFlagSet("check", flag.ExitOnError)
	base := fs.String("url", "http://127.0.0.1:8080", "gateway base URL")
	mixPath := fs.String("mix", "benches/parity/mix.json", "request mix")
	_ = fs.Parse(args)
	client := newClient(1)
	type answer struct {
		Name   string          `json:"name"`
		Status int             `json:"status"`
		Expect int             `json:"expect"`
		Body   json.RawMessage `json:"body"`
	}
	var out []answer
	for _, r := range loadMix(*mixPath, "") {
		resp, err := client.Do(build(*base, r))
		must(err)
		b, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		var buf bytes.Buffer
		if json.Valid(b) {
			buf.Write(b)
		} else {
			q, _ := json.Marshal(string(b))
			buf.Write(q)
		}
		out = append(out, answer{r.Name, resp.StatusCode, r.Expect, buf.Bytes()})
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	must(enc.Encode(out))
}

// runReady polls until the gateway answers the first request of the mix with
// its expected status and prints the wall-clock time of that answer, in
// nanoseconds since the epoch, so a script can subtract its own start time.
func runReady(args []string) {
	fs := flag.NewFlagSet("ready", flag.ExitOnError)
	base := fs.String("url", "http://127.0.0.1:8080", "gateway base URL")
	mixPath := fs.String("mix", "benches/parity/mix.json", "request mix")
	_ = fs.Parse(args)
	r := loadMix(*mixPath, "")[0]
	client := &http.Client{Timeout: time.Second}
	for {
		resp, err := client.Do(build(*base, r))
		if err == nil {
			_, _ = io.Copy(io.Discard, resp.Body)
			resp.Body.Close()
			if resp.StatusCode == r.Expect {
				fmt.Println(time.Now().UnixNano())
				return
			}
		}
		time.Sleep(time.Millisecond)
	}
}
