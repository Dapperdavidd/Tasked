import { useEffect, useMemo, useState } from "react";
import type { FormEvent, ReactElement } from "react";
import {
  ApiClient,
  dayScore,
  defaultSettings,
  saveSettings,
  type ClientSettings,
  type CompletionSummary,
  type Enrollment,
  type IngestJob,
  type NotificationEvent,
  type StatsResponse,
  type TaskInstance,
  type TodayResponse,
  type TodaySection,
  type View
} from "./api";
import type { Cadence } from "../../shared/src/types/Cadence";
import type { Intensity } from "../../shared/src/types/Intensity";

type AppState = {
  view: View;
  settings: ClientSettings;
  today: TodayResponse | null;
  enrollments: Enrollment[];
  stats: StatsResponse | null;
  summary: CompletionSummary | null;
  notifications: NotificationEvent[];
  ingest: IngestJob | null;
  status: string;
  error: string | null;
};

const initialState: AppState = {
  view: "today",
  settings: defaultSettings,
  today: null,
  enrollments: [],
  stats: null,
  summary: null,
  notifications: [],
  ingest: null,
  status: "Ready",
  error: null
};

export function App(): ReactElement {
  const [state, setState] = useState<AppState>(initialState);
  const client = useMemo(() => new ApiClient(state.settings), [state.settings]);
  const program = state.today?.sections.find((section) => section.kind === "Program") ?? null;
  const standing = state.today?.sections.find((section) => section.kind === "Standing") ?? null;

  useEffect(() => {
    void refreshPrimary();
  }, []);

  async function run(success: string, work: () => Promise<Partial<AppState> | void>): Promise<void> {
    setState((current) => ({ ...current, error: null, status: "Working..." }));
    try {
      const patch = await work();
      setState((current) => ({ ...current, ...(patch ?? {}), status: success, error: null }));
    } catch (error) {
      setState((current) => ({
        ...current,
        error: error instanceof Error ? error.message : "Unexpected error",
        status: "Needs attention"
      }));
    }
  }

  async function refreshPrimary(): Promise<void> {
    if (!state.settings.userId.trim()) {
      setState((current) => ({ ...current, status: "Set API details to load data" }));
      return;
    }
    await run("Loaded today", async () => {
      const [today, enrollments, notifications] = await Promise.all([
        client.today(),
        client.enrollments(),
        client.notifications().catch(() => [])
      ]);
      return { today, enrollments, notifications };
    });
  }

  async function changeView(view: View): Promise<void> {
    setState((current) => ({ ...current, view }));
    if (view === "heatmap" && state.stats === null && program) {
      await loadStats(program);
    }
  }

  async function toggleTask(task: TaskInstance): Promise<void> {
    await run("Task updated", async () => {
      if (done(task)) {
        await client.uncompleteTask(task.id);
      } else {
        await client.completeTask(task.id);
      }
      const today = await client.today();
      return { today };
    });
  }

  async function saveNote(dayId: string, note: string): Promise<void> {
    await run("Note saved", async () => {
      await client.updateNote(dayId, note);
    });
  }

  async function repair(dayId: string): Promise<void> {
    await run("Day repaired", async () => {
      await client.repairDay(dayId);
      const today = await client.today();
      return { today };
    });
  }

  async function ingest(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    await run("Ingest checked", async () => {
      const created = await client.createIngest(
        String(form.get("source") ?? ""),
        asIntensity(form.get("intensity")),
        String(form.get("instruction") ?? "")
      );
      let job = await client.ingest(created.job_id);
      for (let attempt = 0; attempt < 8 && job.status !== "ready" && job.status !== "failed"; attempt += 1) {
        await delay(750);
        job = await client.ingest(created.job_id);
      }
      return { ingest: job };
    });
  }

  async function confirmDraft(): Promise<void> {
    const job = state.ingest;
    if (!job || job.status !== "ready") {
      setState((current) => ({ ...current, error: "Draft is not ready." }));
      return;
    }
    await run("Program started", async () => {
      await client.confirmDraft(job.job_id, new Date().toISOString().slice(0, 10), "Africa/Lagos", 0);
      const [today, enrollments] = await Promise.all([client.today(), client.enrollments()]);
      return { view: "today", today, enrollments };
    });
  }

  async function createStanding(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const title = String(form.get("title") ?? "").trim();
    if (!title) {
      setState((current) => ({ ...current, error: "Standing task title is required." }));
      return;
    }
    await run("Standing task added", async () => {
      await client.createStanding(title, standingCadence(String(form.get("cadence") ?? "")), durationBucket(String(form.get("duration") ?? "")));
      const today = await client.today();
      return { today };
    });
  }

  async function loadStats(section = program): Promise<void> {
    if (!section) {
      return;
    }
    const from = state.today?.local_date ?? new Date().toISOString().slice(0, 10);
    const to = addDays(from, 45);
    await run("Stats loaded", async () => {
      const [stats, summary] = await Promise.all([
        client.stats(section.enrollment_id, from, to),
        client.summary(section.enrollment_id).catch(() => null)
      ]);
      return { stats, summary };
    });
  }

  async function testNotification(): Promise<void> {
    await run("Notification queued", async () => {
      await client.enqueueTestNotification();
      const notifications = await client.notifications();
      return { notifications };
    });
  }

  function saveClientSettings(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const next = {
      apiBase: String(form.get("apiBase") ?? "").trim().replace(/\/$/, ""),
      userId: String(form.get("userId") ?? "").trim()
    };
    saveSettings(next);
    setState((current) => ({ ...current, settings: next }));
  }

  return (
    <>
      <Sidebar state={state} program={program} standing={standing} changeView={(view) => void changeView(view)} />
      <main className="main">
        <header className="topbar">
          <div>
            <p className="eyebrow">{formatLongDate(state.today?.local_date)}</p>
            <h1>{pageTitle(state.view)}</h1>
          </div>
          <div className="actions">
            <button className="icon-button" type="button" onClick={() => void refreshPrimary()} title="Refresh">R</button>
            <button className="button subtle" type="button" onClick={() => void changeView("settings")}>Settings</button>
          </div>
        </header>
        {state.error ? <div className="banner error">{state.error}</div> : null}
        <div className="banner">{state.status}</div>
        {state.view === "today" ? (
          <TodayView
            sections={state.today?.sections ?? []}
            program={program}
            standing={standing}
            notifications={state.notifications}
            toggleTask={(task) => void toggleTask(task)}
            saveNote={(dayId, note) => void saveNote(dayId, note)}
            repair={(dayId) => void repair(dayId)}
            testNotification={() => void testNotification()}
            changeView={(view) => void changeView(view)}
            refresh={() => void refreshPrimary()}
          />
        ) : null}
        {state.view === "ingest" ? <IngestView job={state.ingest} submit={(event) => void ingest(event)} confirm={() => void confirmDraft()} /> : null}
        {state.view === "programs" ? <ProgramView enrollments={state.enrollments} program={program} changeView={(view) => void changeView(view)} toggleTask={(task) => void toggleTask(task)} saveNote={(dayId, note) => void saveNote(dayId, note)} repair={(dayId) => void repair(dayId)} /> : null}
        {state.view === "standing" ? <StandingView standing={standing} createStanding={(event) => void createStanding(event)} toggleTask={(task) => void toggleTask(task)} /> : null}
        {state.view === "heatmap" ? <HeatmapView program={program} stats={state.stats} summary={state.summary} loadStats={() => void loadStats()} /> : null}
        {state.view === "cohort" ? <CohortView /> : null}
        {state.view === "settings" ? <SettingsView settings={state.settings} save={saveClientSettings} testNotification={() => void testNotification()} refresh={() => void refreshPrimary()} /> : null}
      </main>
    </>
  );
}

function Sidebar(props: { state: AppState; program: TodaySection | null; standing: TodaySection | null; changeView: (view: View) => void }): ReactElement {
  return (
    <aside className="sidebar">
      <div className="brand"><span className="logo">✓</span><span>TRACKED</span></div>
      <nav className="nav">
        {navItems.map((item) => (
          <button key={item.view} className={`nav-item ${props.state.view === item.view ? "active" : ""}`} type="button" onClick={() => props.changeView(item.view)}>
            <span>{item.icon}</span>{item.label}
          </button>
        ))}
      </nav>
      <SidebarCard label="Active program" title={props.program?.title ?? "No active program"} detail={progressLabel(props.program)} />
      <SidebarCard label="Standing list" title={props.standing ? `${props.standing.tasks.length}/5 used` : "0/5 used"} detail={props.standing ? `${props.standing.tasks.filter(done).length} logged today` : "Add a baseline"} />
    </aside>
  );
}

const navItems: Array<{ view: View; label: string; icon: string }> = [
  { view: "today", label: "Today", icon: "□" },
  { view: "ingest", label: "Ingest", icon: "+" },
  { view: "programs", label: "Program", icon: "▤" },
  { view: "standing", label: "Standing", icon: "○" },
  { view: "heatmap", label: "Heatmap", icon: "▦" },
  { view: "cohort", label: "Cohort", icon: "◇" },
  { view: "settings", label: "Settings", icon: "⚙" }
];

function SidebarCard(props: { label: string; title: string; detail: string }): ReactElement {
  return <div className="sidebar-card"><p>{props.label}</p><strong>{props.title}</strong><span>{props.detail}</span></div>;
}

function TodayView(props: {
  sections: TodaySection[];
  program: TodaySection | null;
  standing: TodaySection | null;
  notifications: NotificationEvent[];
  toggleTask: (task: TaskInstance) => void;
  saveNote: (dayId: string, note: string) => void;
  repair: (dayId: string) => void;
  testNotification: () => void;
  changeView: (view: View) => void;
  refresh: () => void;
}): ReactElement {
  return (
    <>
      <section className="metrics">
        <Metric label="Daily score" value={props.program ? String(dayScore(props.program)) : "0"} sub="Great day" />
        <Metric label="Streak" value={props.program ? `${props.program.streak.current} days` : "0 days"} sub={props.program ? `Longest ${props.program.streak.longest}` : "No active program"} />
        <Metric label="Program progress" value={progressLabel(props.program)} sub={props.program ? `${props.program.tasks.filter(done).length}/${props.program.tasks.length} tasks` : "Start a program"} />
        <Metric label="Standing" value={props.standing ? `${props.standing.tasks.filter(done).length}/${props.standing.tasks.length}` : "0/5"} sub="baseline tasks" />
      </section>
      <section className="split">
        <div className="plan-panel">
          <div className="panel-head">
            <div>
              <h2>Today's plan</h2>
              <p>{props.sections.length} section{props.sections.length === 1 ? "" : "s"}</p>
            </div>
            <button className="button subtle" type="button" onClick={props.refresh}>Refresh</button>
          </div>
          {props.program ? <SectionCard section={props.program} toggleTask={props.toggleTask} saveNote={props.saveNote} repair={props.repair} /> : <EmptyProgram changeView={props.changeView} />}
          {props.standing ? <SectionCard section={props.standing} toggleTask={props.toggleTask} saveNote={props.saveNote} repair={props.repair} /> : <EmptyStanding changeView={props.changeView} />}
        </div>
        <aside className="rail">
          <MiniHeatmap section={props.program} changeView={props.changeView} />
          <NotificationsPanel notifications={props.notifications} testNotification={props.testNotification} />
        </aside>
      </section>
    </>
  );
}

function SectionCard(props: { section: TodaySection; toggleTask: (task: TaskInstance) => void; saveNote: (dayId: string, note: string) => void; repair: (dayId: string) => void }): ReactElement {
  const completed = props.section.tasks.filter(done).length;
  const sorted = [...props.section.tasks].sort((a, b) => Number(done(a)) - Number(done(b)) || a.position - b.position);
  const isProgram = props.section.kind === "Program";
  return (
    <article className={`section-card ${isProgram ? "primary" : "quiet"}`}>
      <div className="section-title">
        <div>
          <p>{isProgram ? "Program tasks" : "Standing list"}</p>
          <h3>{props.section.title}</h3>
        </div>
        <span>{completed}/{props.section.tasks.length}</span>
      </div>
      {isProgram ? <p className="counter">{dayCounter(props.section)}</p> : null}
      <div className="tasks">
        {sorted.map((task) => (
          <button key={task.id} className={`task-row ${done(task) ? "done" : ""}`} type="button" onClick={() => props.toggleTask(task)}>
            <span className="check">{done(task) ? "✓" : ""}</span>
            <span className="task-title">{task.title}</span>
            <span className="task-meta">{task.points} pts</span>
          </button>
        ))}
      </div>
      {isProgram ? (
        <>
          <label className="note-label">Add a note</label>
          <textarea className="note" rows={2} defaultValue={props.section.note ?? ""} placeholder="How did today go?" onBlur={(event) => props.saveNote(props.section.day_id, event.currentTarget.value)} />
        </>
      ) : null}
      {props.section.streak.state === "Repairable" ? <button className="button repair" type="button" onClick={() => props.repair(props.section.day_id)}>Repair yesterday</button> : null}
    </article>
  );
}

function IngestView(props: { job: IngestJob | null; submit: (event: FormEvent<HTMLFormElement>) => void; confirm: () => void }): ReactElement {
  const draft = props.job?.draft ?? null;
  return (
    <section className="split">
      <div className="panel">
        <h2>Ingest anything</h2>
        <form className="form" onSubmit={props.submit}>
          <textarea name="source" rows={9} placeholder="Paste a syllabus, training plan, project checklist, or two sentence plan." />
          <input name="instruction" placeholder="Optional instruction" />
          <div className="segmented" role="group">
            <IntensityRadio value="light" label="Light" />
            <IntensityRadio value="standard" label="Standard" />
            <IntensityRadio value="heavy" label="Heavy" />
          </div>
          <button className="button primary" type="submit">Generate draft</button>
        </form>
        {props.job ? <p className="job-status">Job {props.job.job_id}: {props.job.status}</p> : null}
      </div>
      <div className="panel">
        <div className="panel-head">
          <div>
            <h2>{draft ? draft.title : "Draft preview"}</h2>
            <p>{draft ? `${draft.duration_days} days · ${draft.tasks.length} tasks` : "Waiting for a ready ingest job"}</p>
          </div>
          {draft ? <button className="button primary" type="button" onClick={props.confirm}>Start Day 1</button> : null}
        </div>
        {draft ? draft.tasks.map((task, index) => (
          <div className="preview-row" key={`${task.title}-${index}`}>
            <span>Day {index + 1}</span>
            <strong>{task.title}</strong>
            <span>{task.estimated_minutes}m</span>
          </div>
        )) : <div className="empty">Generate a draft to review the program before confirming.</div>}
      </div>
    </section>
  );
}

function ProgramView(props: { enrollments: Enrollment[]; program: TodaySection | null; changeView: (view: View) => void; toggleTask: (task: TaskInstance) => void; saveNote: (dayId: string, note: string) => void; repair: (dayId: string) => void }): ReactElement {
  const bounded = props.enrollments.filter((item) => !item.is_standing);
  return (
    <section className="panel">
      <div className="panel-head">
        <div>
          <h2>Programs</h2>
          <p>{bounded.length} bounded enrollment{bounded.length === 1 ? "" : "s"}</p>
        </div>
        <button className="button primary" type="button" onClick={() => props.changeView("ingest")}>New program</button>
      </div>
      {props.program ? <SectionCard section={props.program} toggleTask={props.toggleTask} saveNote={props.saveNote} repair={props.repair} /> : <EmptyProgram changeView={props.changeView} />}
      <div className="list">
        {bounded.map((enrollment) => <div className="list-row" key={enrollment.id}><strong>{enrollment.status}</strong><span>{enrollment.start_date}</span><span>{enrollment.timezone}</span></div>)}
      </div>
    </section>
  );
}

function StandingView(props: { standing: TodaySection | null; createStanding: (event: FormEvent<HTMLFormElement>) => void; toggleTask: (task: TaskInstance) => void }): ReactElement {
  return (
    <section className="panel narrow">
      <div className="panel-head">
        <div>
          <h2>Five slots</h2>
          <p>{props.standing ? props.standing.tasks.length : 0}/5 used</p>
        </div>
      </div>
      {props.standing ? <SectionCard section={props.standing} toggleTask={props.toggleTask} saveNote={() => undefined} repair={() => undefined} /> : <EmptyStanding changeView={() => undefined} />}
      <form className="form" onSubmit={props.createStanding}>
        <input name="title" placeholder="Task title" />
        <select name="cadence">
          <option value="daily">Every day</option>
          <option value="weekdays">Weekdays</option>
          <option value="three">3 times a week</option>
        </select>
        <select name="duration">
          <option value="five">5 minutes</option>
          <option value="ten">10 minutes</option>
          <option value="fifteen">15 minutes</option>
          <option value="thirty">30 minutes</option>
        </select>
        <button className="button primary" type="submit">Add standing task</button>
      </form>
    </section>
  );
}

function HeatmapView(props: { program: TodaySection | null; stats: StatsResponse | null; summary: CompletionSummary | null; loadStats: () => void }): ReactElement {
  return (
    <section className="split">
      <div className="panel">
        <h2>{props.program ? props.program.title : "Heatmap"}</h2>
        <MiniHeatmap section={props.program} changeView={() => undefined} />
        {props.summary ? <p className="counter">{props.summary.payload.days_logged}/{props.summary.payload.days_total} days logged · longest {props.summary.payload.longest_streak}</p> : null}
      </div>
      <div className="panel">
        <h2>Task completion</h2>
        {props.stats ? props.stats.tasks.map((task) => {
          const rate = task.available_count === 0 ? 0 : Math.round((task.completed_count / task.available_count) * 100);
          return <div className="list-row" key={task.template_id}><strong>{task.title}</strong><span>{rate}%</span><span>{task.completed_count}/{task.available_count}</span></div>;
        }) : <button className="button primary" type="button" onClick={props.loadStats}>Load stats</button>}
      </div>
    </section>
  );
}

function CohortView(): ReactElement {
  return <section className="panel narrow"><h2>Cohort</h2><div className="empty">Presence endpoints are ready. Invite creation and member rows are the next UI layer for this tab.</div></section>;
}

function SettingsView(props: { settings: ClientSettings; save: (event: FormEvent<HTMLFormElement>) => void; testNotification: () => void; refresh: () => void }): ReactElement {
  return (
    <section className="panel narrow">
      <h2>Settings</h2>
      <form className="form" onSubmit={props.save}>
        <label>API base<input name="apiBase" defaultValue={props.settings.apiBase} /></label>
        <label>User ID<input name="userId" defaultValue={props.settings.userId} /></label>
        <button className="button primary" type="submit">Save settings</button>
      </form>
      <div className="list">
        <button className="button subtle" type="button" onClick={props.testNotification}>Queue test notification</button>
        <button className="button subtle" type="button" onClick={props.refresh}>Reload data</button>
      </div>
    </section>
  );
}

function NotificationsPanel(props: { notifications: NotificationEvent[]; testNotification: () => void }): ReactElement {
  return (
    <div className="panel compact">
      <div className="panel-head">
        <h2>Notifications</h2>
        <button className="button subtle" type="button" onClick={props.testNotification}>Test</button>
      </div>
      {props.notifications.slice(0, 4).map((event) => (
        <div className="notification-row" key={event.id}>
          <strong>{event.title}</strong>
          <span>{event.status}</span>
          <p>{event.body}</p>
        </div>
      ))}
      {props.notifications.length === 0 ? <div className="empty">No notification events yet.</div> : null}
    </div>
  );
}

function MiniHeatmap(props: { section: TodaySection | null; changeView: (view: View) => void }): ReactElement {
  const score = props.section ? dayScore(props.section) : 0;
  const cells = Array.from({ length: 56 }, (_, index) => index === 11 ? score : Math.max(0, Math.min(100, (index * 17) % 110)));
  return (
    <div className="panel compact">
      <div className="panel-head">
        <div>
          <h2>Heatmap</h2>
          <p>{props.section ? props.section.title : "No active program"}</p>
        </div>
        <button className="button subtle" type="button" onClick={() => props.changeView("heatmap")}>View</button>
      </div>
      <div className="heatmap">
        {cells.map((value, index) => <span className={`cell ${heatClass(value)}`} key={index} />)}
      </div>
      <div className="legend"><span>Missed</span><span>Low</span><span>Medium</span><span>High</span><span>Perfect</span></div>
    </div>
  );
}

function Metric(props: { label: string; value: string; sub: string }): ReactElement {
  return <div className="metric"><span>{props.label}</span><strong>{props.value}</strong><p>{props.sub}</p></div>;
}

function EmptyProgram(props: { changeView: (view: View) => void }): ReactElement {
  return <div className="empty"><strong>No active program.</strong><button className="button primary" type="button" onClick={() => props.changeView("ingest")}>Start one</button></div>;
}

function EmptyStanding(props: { changeView: (view: View) => void }): ReactElement {
  return <div className="empty"><strong>Standing list is empty.</strong><button className="button subtle" type="button" onClick={() => props.changeView("standing")}>Add up to five</button></div>;
}

function IntensityRadio(props: { value: Intensity; label: string }): ReactElement {
  return <label><input type="radio" name="intensity" value={props.value} defaultChecked={props.value === "standard"} />{props.label}</label>;
}

function pageTitle(view: View): string {
  switch (view) {
    case "today":
      return "Today";
    case "programs":
      return "Program";
    case "ingest":
      return "Review program";
    case "standing":
      return "Standing list";
    case "heatmap":
      return "Heatmap";
    case "cohort":
      return "Cohort";
    case "settings":
      return "Settings";
  }
}

function done(task: { completed_at: string | null }): boolean {
  return task.completed_at !== null;
}

function progressLabel(section: TodaySection | null): string {
  if (!section || section.day_index === null || section.duration_days === null) {
    return "No bounded run";
  }
  return `Day ${section.day_index + 1} of ${section.duration_days}`;
}

function dayCounter(section: TodaySection): string {
  if (section.day_index === null || section.duration_days === null) {
    return "";
  }
  const day = section.day_index + 1;
  const remain = Math.max(section.duration_days - day, 0);
  return `Day ${day} of ${section.duration_days}, ${remain} remain`;
}

function heatClass(value: number): string {
  if (value >= 95) {
    return "perfect";
  }
  if (value >= 80) {
    return "high";
  }
  if (value >= 50) {
    return "medium";
  }
  if (value > 0) {
    return "low";
  }
  return "missed";
}

function asIntensity(value: FormDataEntryValue | null): Intensity {
  if (value === "light" || value === "heavy") {
    return value;
  }
  return "standard";
}

function standingCadence(value: string): Cadence {
  if (value === "weekdays") {
    return { type: "weekly_days", days: [1, 2, 3, 4, 5] };
  }
  if (value === "three") {
    return { type: "n_per_week", count: 3 };
  }
  return { type: "daily" };
}

function durationBucket(value: string): "five" | "ten" | "fifteen" | "thirty" {
  if (value === "ten" || value === "fifteen" || value === "thirty") {
    return value;
  }
  return "five";
}

function addDays(date: string, days: number): string {
  const next = new Date(`${date}T00:00:00Z`);
  next.setUTCDate(next.getUTCDate() + days);
  return next.toISOString().slice(0, 10);
}

function formatLongDate(date: string | undefined): string {
  if (!date) {
    return "Not loaded";
  }
  return new Intl.DateTimeFormat("en", { weekday: "long", month: "long", day: "numeric", year: "numeric" }).format(new Date(`${date}T00:00:00Z`));
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms);
  });
}
