import { useEffect, useMemo, useState } from "react";
import type { ChangeEvent, FormEvent, ReactElement } from "react";
import {
  ApiClient,
  clearSessionSettings,
  dayScore,
  defaultSettings,
  saveSettings,
  type ClientSettings,
  type CompletionSummary,
  type CohortPresence,
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

type Theme = "dark" | "light";

type AppState = {
  view: View;
  settings: ClientSettings;
  today: TodayResponse | null;
  enrollments: Enrollment[];
  stats: StatsResponse | null;
  summary: CompletionSummary | null;
  cohortPresence: CohortPresence[];
  inviteToken: string | null;
  notifications: NotificationEvent[];
  ingest: IngestJob | null;
  status: string;
  error: { view: View; message: string } | null;
};

const initialState: AppState = {
  view: "today",
  settings: defaultSettings,
  today: null,
  enrollments: [],
  stats: null,
  summary: null,
  cohortPresence: [],
  inviteToken: null,
  notifications: [],
  ingest: null,
  status: "Ready",
  error: null
};

export function App(): ReactElement {
  const [state, setState] = useState<AppState>(initialState);
  const [profileName, setProfileName] = useState(storedProfileName);
  const [theme, setTheme] = useState<Theme>(storedTheme);
  const client = useMemo(() => new ApiClient(state.settings), [state.settings]);
  const program = state.today?.sections.find((section) => section.kind === "Program") ?? null;
  const standing = state.today?.sections.find((section) => section.kind === "Standing") ?? null;
  const activeEnrollment = program ? state.enrollments.find((enrollment) => enrollment.id === program.enrollment_id) ?? null : null;
  const [returnDismissedFor, setReturnDismissedFor] = useState<string | null>(null);
  const showReturn = state.today?.lapsed_days !== null && state.today?.lapsed_days !== undefined && returnDismissedFor !== state.today.local_date;

  useEffect(() => {
    void boot();
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("tracked.theme", theme);
  }, [theme]);

  async function boot(): Promise<void> {
    if (state.settings.userId.trim()) {
      await run("Loaded today", async () => {
        try {
          return await loadPrimary(client);
        } catch {
          clearSessionSettings();
          const fallbackSettings = { apiBase: "", userId: "" };
          const fallbackClient = new ApiClient(fallbackSettings);
          const session = await fallbackClient.createSession(undefined, profileName);
          const nextSettings = { apiBase: "", userId: session.user_id };
          saveSettings(nextSettings);
          saveProfileName(session.display_name);
          setProfileName(session.display_name);
          const nextClient = new ApiClient(nextSettings);
          const patch = await loadPrimary(nextClient);
          return { ...patch, settings: nextSettings };
        }
      });
      return;
    }

    await run("Session ready", async () => {
      const session = await client.createSession(undefined, profileName);
      const nextSettings = { ...state.settings, userId: session.user_id };
      saveSettings(nextSettings);
      saveProfileName(session.display_name);
      setProfileName(session.display_name);
      const nextClient = new ApiClient(nextSettings);
      const patch = await loadPrimary(nextClient);
      return { ...patch, settings: nextSettings };
    });
  }

  async function run(success: string, work: () => Promise<Partial<AppState> | void>): Promise<void> {
    const view = state.view;
    setState((current) => ({ ...current, error: null, status: "Working..." }));
    try {
      const patch = await work();
      setState((current) => ({ ...current, ...(patch ?? {}), status: success, error: null }));
    } catch (error) {
      setState((current) => ({
        ...current,
        error: { view, message: error instanceof Error ? error.message : "Unexpected error" },
        status: "Needs attention"
      }));
    }
  }

  function setPageError(message: string): void {
    const view = state.view;
    setState((current) => ({ ...current, error: { view, message }, status: "Needs attention" }));
  }

  async function refreshPrimary(): Promise<void> {
    if (!state.settings.userId.trim()) {
      await boot();
      return;
    }
    await run("Loaded today", async () => {
      return loadPrimary(client);
    });
  }

  async function loadPrimary(loadClient: ApiClient): Promise<Partial<AppState>> {
    const [today, enrollments, notifications] = await Promise.all([
      loadClient.today(),
      loadClient.enrollments(),
      loadClient.notifications().catch(() => [])
    ]);
    const active = today.sections.find((section) => section.kind === "Program") ?? null;
    const enrollment = active ? enrollments.find((item) => item.id === active.enrollment_id) ?? null : null;
    const cohortPresence = enrollment?.cohort_id ? await loadClient.cohortPresence(enrollment.cohort_id, today.local_date).catch(() => []) : [];
    return { today, enrollments, notifications, cohortPresence };
  }

  async function changeView(view: View): Promise<void> {
    setState((current) => ({ ...current, view, error: null }));
    if (view === "heatmap" && state.stats === null && program) {
      await loadStats(program);
    }
    if (view === "cohort" && activeEnrollment?.cohort_id && state.today) {
      await loadCohort(activeEnrollment.cohort_id, state.today.local_date);
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
      setPageError("Your plan is not ready yet.");
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
      setPageError("Standing task title is required.");
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

  async function patchEnrollment(id: string, status: "active" | "paused" | "completed" | "abandoned"): Promise<void> {
    await run("Enrollment updated", async () => {
      await client.patchEnrollment(id, { status });
      const [today, enrollments] = await Promise.all([client.today(), client.enrollments()]);
      return { today, enrollments };
    });
  }

  async function returnEnrollment(id: string, action: "resume" | "restart" | "scale_down"): Promise<void> {
    await run(returnActionStatus(action), async () => {
      await client.returnEnrollment(id, action);
      const [today, enrollments] = await Promise.all([client.today(), client.enrollments()]);
      return { today, enrollments };
    });
  }

  async function createCohort(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (!activeEnrollment) {
      setPageError("Start a program before creating a cohort.");
      return;
    }
    const form = new FormData(event.currentTarget);
    await run("Cohort created", async () => {
      const cohort = await client.createCohort(activeEnrollment.program_id, String(form.get("name") ?? ""));
      const invite = await client.createInvite(cohort.id);
      const [today, enrollments] = await Promise.all([client.today(), client.enrollments()]);
      const cohortPresence = await client.cohortPresence(cohort.id, today.local_date).catch(() => []);
      return { today, enrollments, inviteToken: invite.token, cohortPresence };
    });
  }

  async function createInvite(): Promise<void> {
    if (!activeEnrollment?.cohort_id) {
      setPageError("Create a cohort first.");
      return;
    }
    await run("Invite created", async () => {
      const invite = await client.createInvite(activeEnrollment.cohort_id ?? "");
      return { inviteToken: invite.token };
    });
  }

  async function joinCohort(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const token = String(form.get("token") ?? "").trim();
    if (!token) {
      setPageError("Invite token is required.");
      return;
    }
    await run("Joined cohort", async () => {
      const joined = await client.joinCohort(token);
      const [today, enrollments] = await Promise.all([client.today(), client.enrollments()]);
      const cohortPresence = await client.cohortPresence(joined.cohort_id, today.local_date).catch(() => []);
      return { today, enrollments, cohortPresence };
    });
  }

  async function loadCohort(cohortId: string, localDate: string): Promise<void> {
    await run("Cohort loaded", async () => {
      const cohortPresence = await client.cohortPresence(cohortId, localDate);
      return { cohortPresence };
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

  function saveProfile(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const name = String(form.get("displayName") ?? "").trim() || "Dapper";
    const nextTheme = form.get("theme") === "light" ? "light" : "dark";
    saveProfileName(name);
    setProfileName(name);
    setTheme(nextTheme);
    setState((current) => ({ ...current, status: "Profile saved", error: null }));
  }

  return (
    <>
      <Sidebar state={state} program={program} standing={standing} profileName={profileName} changeView={(view) => void changeView(view)} />
      <main className="main">
        <header className="topbar">
          <div>
            <p className="eyebrow">{formatLongDate(state.today?.local_date)}</p>
            <h1>{pageTitle(state.view)}</h1>
          </div>
          <div className="actions">
            <button className="profile-button" type="button" onClick={() => void changeView("profile")} title="Profile">{initials(profileName)}</button>
          </div>
        </header>
        {state.error?.view === state.view ? <div className="banner error">{state.error.message}</div> : null}
        {state.status !== "Ready" ? <div className="banner">{state.status}</div> : null}
        {state.view === "today" ? (
          showReturn ? (
            <ReturnView
              lapsedDays={state.today?.lapsed_days ?? 0}
              program={program}
              activeEnrollment={activeEnrollment}
              resume={() => {
                setReturnDismissedFor(state.today?.local_date ?? null);
                if (activeEnrollment) {
                  void returnEnrollment(activeEnrollment.id, "resume");
                }
              }}
              restart={() => activeEnrollment ? void returnEnrollment(activeEnrollment.id, "restart") : undefined}
              scaleDown={() => activeEnrollment ? void returnEnrollment(activeEnrollment.id, "scale_down") : undefined}
            />
          ) : <TodayView
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
        {state.view === "ingest" ? <IngestView job={state.ingest} submit={(event) => void ingest(event)} confirm={() => void confirmDraft()} extractFile={(file) => client.extractFile(file).then((response) => response.text)} /> : null}
        {state.view === "programs" ? <ProgramView enrollments={state.enrollments} program={program} activeEnrollment={activeEnrollment} changeView={(view) => void changeView(view)} patchEnrollment={(id, status) => void patchEnrollment(id, status)} toggleTask={(task) => void toggleTask(task)} saveNote={(dayId, note) => void saveNote(dayId, note)} repair={(dayId) => void repair(dayId)} /> : null}
        {state.view === "standing" ? <StandingView standing={standing} createStanding={(event) => void createStanding(event)} toggleTask={(task) => void toggleTask(task)} /> : null}
        {state.view === "heatmap" ? <HeatmapView program={program} stats={state.stats} summary={state.summary} loadStats={() => void loadStats()} /> : null}
        {state.view === "cohort" ? <CohortView activeEnrollment={activeEnrollment} presence={state.cohortPresence} inviteToken={state.inviteToken} createCohort={(event) => void createCohort(event)} createInvite={() => void createInvite()} joinCohort={(event) => void joinCohort(event)} /> : null}
        {state.view === "profile" ? <ProfileView settings={state.settings} profileName={profileName} theme={theme} saveProfile={saveProfile} saveAdvanced={saveClientSettings} testNotification={() => void testNotification()} refresh={() => void refreshPrimary()} /> : null}
      </main>
    </>
  );
}

function Sidebar(props: { state: AppState; program: TodaySection | null; standing: TodaySection | null; profileName: string; changeView: (view: View) => void }): ReactElement {
  return (
    <aside className="sidebar">
      <div className="brand"><span className="logo">✓</span><span>TRACKED</span></div>
      <nav className="nav">
        {navItems.map((item) => (
          <button key={item.view} className={`nav-item ${props.state.view === item.view ? "active" : ""}`} type="button" onClick={() => props.changeView(item.view)}>
            <span>{item.view === "profile" ? initials(props.profileName) : item.icon}</span>{item.label}
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
  { view: "profile", label: "Profile", icon: "●" }
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
          {props.standing && props.standing.tasks.length > 0 ? <SectionCard section={props.standing} toggleTask={props.toggleTask} saveNote={props.saveNote} repair={props.repair} /> : <EmptyStanding changeView={props.changeView} />}
        </div>
        <aside className="rail">
          <MiniHeatmap section={props.program} changeView={props.changeView} />
          <NotificationsPanel notifications={props.notifications} testNotification={props.testNotification} />
        </aside>
      </section>
    </>
  );
}

function ReturnView(props: {
  lapsedDays: number;
  program: TodaySection | null;
  activeEnrollment: Enrollment | null;
  resume: () => void;
  restart: () => void;
  scaleDown: () => void;
}): ReactElement {
  return (
    <section className="return-screen">
      <div className="panel return-panel">
        <p className="eyebrow">Return</p>
        <h2>{props.program ? props.program.title : "Pick up where you left off"}</h2>
        <p>{props.lapsedDays} days away. Your best run is still preserved.</p>
        <div className="artifact">
          <Metric label="Longest streak" value={props.program ? String(props.program.streak.longest) : "0"} sub="preserved" />
          <Metric label="Current run" value={props.program ? String(props.program.streak.current) : "0"} sub="today" />
          <Metric label="Status" value={props.activeEnrollment?.status ?? "No program"} sub="bounded enrollment" />
        </div>
        <div className="return-actions">
          <button className="button primary" type="button" onClick={props.resume}>Resume from today</button>
          <button className="button subtle" type="button" onClick={props.restart}>Restart at day one</button>
          <button className="button subtle" type="button" onClick={props.scaleDown}>Scale down</button>
        </div>
      </div>
    </section>
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

function IngestView(props: { job: IngestJob | null; submit: (event: FormEvent<HTMLFormElement>) => void; confirm: () => void; extractFile: (file: File) => Promise<string> }): ReactElement {
  const draft = props.job?.draft ?? null;
  const blockedUpload = props.job?.status === "failed" && props.job.error_code === "needs_ocr";
  return (
    <section className="split">
      <div className="panel">
        <h2>Create a program</h2>
        <form className="form" onSubmit={props.submit}>
          <label className="upload-button">
            <span>+</span>
            <input type="file" accept=".txt,.md,.csv,.json,.pdf,.docx,text/*,application/pdf,application/vnd.openxmlformats-officedocument.wordprocessingml.document,image/*" onChange={(event) => void readUpload(event, props.extractFile)} />
            Add document
          </label>
          <textarea id="source-text" name="source" rows={7} placeholder="Paste a syllabus, training plan, checklist, or short goal." />
          <input name="instruction" placeholder="Name it or add direction, e.g. 8 week 5K plan" />
          <div className="segmented" role="group">
            <IntensityRadio value="light" label="Light" />
            <IntensityRadio value="standard" label="Standard" />
            <IntensityRadio value="heavy" label="Heavy" />
          </div>
          <button className="button primary" type="submit">Generate plan</button>
        </form>
        {props.job ? <p className="job-status">{jobLabel(props.job)}</p> : null}
        {blockedUpload ? <p className="job-status">Images and scanned PDFs need OCR support. For now, paste extracted text or upload a text document.</p> : null}
      </div>
      <div className="panel">
        <div className="panel-head">
          <div>
            <h2>{draft ? draft.title : "Generated plan"}</h2>
            <p>{draft ? `${draft.duration_days} days · ${draft.tasks.length} tasks` : "Your generated plan appears here."}</p>
          </div>
          {draft ? <button className="button primary" type="button" onClick={props.confirm}>Start Day 1</button> : null}
        </div>
        {draft ? draft.tasks.map((task, index) => (
          <div className="preview-row" key={`${task.title}-${index}`}>
            <span>Day {index + 1}</span>
            <strong>{task.title}</strong>
            <span>{task.estimated_minutes}m</span>
          </div>
        )) : <div className="empty">Nothing generated yet.</div>}
      </div>
    </section>
  );
}

function ProgramView(props: {
  enrollments: Enrollment[];
  program: TodaySection | null;
  activeEnrollment: Enrollment | null;
  changeView: (view: View) => void;
  patchEnrollment: (id: string, status: "active" | "paused" | "completed" | "abandoned") => void;
  toggleTask: (task: TaskInstance) => void;
  saveNote: (dayId: string, note: string) => void;
  repair: (dayId: string) => void;
}): ReactElement {
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
      {props.activeEnrollment ? (
        <div className="action-row">
          <button className="button subtle" type="button" onClick={() => props.patchEnrollment(props.activeEnrollment?.id ?? "", props.activeEnrollment?.status === "Paused" ? "active" : "paused")}>
            {props.activeEnrollment.status === "Paused" ? "Resume" : "Pause"}
          </button>
          <button className="button subtle" type="button" onClick={() => props.patchEnrollment(props.activeEnrollment?.id ?? "", "completed")}>Mark complete</button>
          <button className="button danger" type="button" onClick={() => props.patchEnrollment(props.activeEnrollment?.id ?? "", "abandoned")}>Abandon</button>
        </div>
      ) : null}
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
      {props.standing && props.standing.tasks.length > 0 ? <SectionCard section={props.standing} toggleTask={props.toggleTask} saveNote={() => undefined} repair={() => undefined} /> : <EmptyStanding changeView={() => undefined} />}
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
  const days = props.stats?.days ?? [];
  return (
    <section className="split">
      <div className="panel">
        <h2>{props.program ? props.program.title : "Heatmap"}</h2>
        <HeatmapGrid days={days} fallbackSection={props.program} />
        {props.summary ? <p className="counter">{props.summary.payload.days_logged}/{props.summary.payload.days_total} days logged · longest {props.summary.payload.longest_streak}</p> : null}
        {props.summary ? (
          <div className="artifact">
            <Metric label="Completion" value={props.summary.payload.completion_rate === null ? "0%" : `${Math.round(props.summary.payload.completion_rate)}%`} sub="finalised days" />
            <Metric label="Tasks" value={String(props.summary.payload.tasks_completed)} sub="completed" />
            <Metric label="Hours" value={String(Math.round(props.summary.payload.hours_invested))} sub="invested" />
          </div>
        ) : null}
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

function CohortView(props: {
  activeEnrollment: Enrollment | null;
  presence: CohortPresence[];
  inviteToken: string | null;
  createCohort: (event: FormEvent<HTMLFormElement>) => void;
  createInvite: () => void;
  joinCohort: (event: FormEvent<HTMLFormElement>) => void;
}): ReactElement {
  const hasCohort = props.activeEnrollment?.cohort_id !== null && props.activeEnrollment?.cohort_id !== undefined;
  return (
    <section className="split">
      <div className="panel narrow">
        <div className="panel-head">
          <div>
            <h2>{hasCohort ? "Cohort presence" : "Create a cohort"}</h2>
            <p>Presence only. No task titles, notes, or standing list data.</p>
          </div>
          {hasCohort ? <button className="button subtle" type="button" onClick={props.createInvite}>Invite</button> : null}
        </div>
        {hasCohort ? (
          <div className="presence-list">
            {props.presence.map((member) => (
              <div className="presence-row" key={member.user_id}>
                <span className={`avatar-dot ${member.logged_today ? "online" : ""}`}>{initials(member.display_name ?? "You")}</span>
                <div>
                  <strong>{member.display_name ?? "Member"}</strong>
                  <p>{member.logged_today ? "Logged today" : "Not logged today"}</p>
                </div>
                <span>{member.streak} days</span>
              </div>
            ))}
            {props.presence.length === 0 ? <div className="empty">No presence rows yet.</div> : null}
          </div>
        ) : (
          <form className="form" onSubmit={props.createCohort}>
            <input name="name" placeholder="Cohort name, e.g. 5K Crew" />
            <button className="button primary" type="submit">Create cohort</button>
          </form>
        )}
        {props.inviteToken ? <div className="invite-token"><span>Invite token</span><code>{props.inviteToken}</code></div> : null}
      </div>
      <div className="panel narrow">
        <h2>Join with token</h2>
        <form className="form" onSubmit={props.joinCohort}>
          <input name="token" placeholder="Paste invite token" />
          <button className="button primary" type="submit">Join cohort</button>
        </form>
      </div>
    </section>
  );
}

function ProfileView(props: {
  settings: ClientSettings;
  profileName: string;
  theme: Theme;
  saveProfile: (event: FormEvent<HTMLFormElement>) => void;
  saveAdvanced: (event: FormEvent<HTMLFormElement>) => void;
  testNotification: () => void;
  refresh: () => void;
}): ReactElement {
  return (
    <section className="profile-grid">
      <div className="panel profile-card">
        <div className="profile-hero">
          <span className="profile-avatar">{initials(props.profileName)}</span>
          <div>
            <h2>{props.profileName}</h2>
            <p>Google profile will connect here later.</p>
          </div>
        </div>
        <form className="form" onSubmit={props.saveProfile}>
          <label>Display name<input name="displayName" defaultValue={props.profileName} placeholder="Your name" /></label>
          <div className="segmented compact" role="group">
            <label><input type="radio" name="theme" value="dark" defaultChecked={props.theme === "dark"} />Dark</label>
            <label><input type="radio" name="theme" value="light" defaultChecked={props.theme === "light"} />Light</label>
          </div>
          <button className="button primary" type="submit">Save profile</button>
        </form>
      </div>
      <div className="panel">
        <h2>Account</h2>
        <div className="profile-actions">
          <button className="button subtle" type="button" onClick={props.refresh}>Reload data</button>
          <button className="button subtle" type="button" onClick={props.testNotification}>Test notification</button>
          <button className="button subtle" type="button">Manage tasks</button>
          <button className="button danger" type="button">Delete account</button>
        </div>
      </div>
      <details className="panel advanced-panel">
        <summary>Advanced API settings</summary>
        <form className="form" onSubmit={props.saveAdvanced}>
          <label>API base<input name="apiBase" defaultValue={props.settings.apiBase} placeholder="Leave blank for automatic" /></label>
          <input name="userId" type="hidden" value={props.settings.userId} readOnly />
          <button className="button subtle" type="submit">Save advanced setting</button>
        </form>
      </details>
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
  if (!props.section) {
    return (
      <div className="panel compact">
        <div className="panel-head">
          <div>
            <h2>Heatmap</h2>
            <p>No program yet</p>
          </div>
        </div>
        <div className="empty">Start a program to build your heatmap.</div>
      </div>
    );
  }
  const score = props.section ? dayScore(props.section) : 0;
  const cells = Array.from({ length: 56 }, (_, index) => index === 11 ? score : 0);
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

function HeatmapGrid(props: { days: StatsResponse["days"]; fallbackSection: TodaySection | null }): ReactElement {
  if (props.days.length === 0) {
    return <MiniHeatmap section={props.fallbackSection} changeView={() => undefined} />;
  }
  return (
    <>
      <div className="heatmap-shell">
        <div className="heatmap-weekdays"><span>Mon</span><span>Wed</span><span>Fri</span></div>
        <div className="heatmap full">
          {props.days.map((day) => <span className={`cell ${statusClass(day.status, scoreFromDay(day))}`} title={`${day.local_date} · ${day.status}`} key={day.id} />)}
        </div>
      </div>
      <div className="legend"><span>Missed</span><span>Low</span><span>Medium</span><span>High</span><span>Perfect</span></div>
    </>
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

async function readUpload(event: ChangeEvent<HTMLInputElement>, extractFile: (file: File) => Promise<string>): Promise<void> {
  const file = event.currentTarget.files?.[0];
  if (!file) {
    return;
  }

  const target = document.getElementById("source-text");
  if (!(target instanceof HTMLTextAreaElement)) {
    return;
  }

  target.placeholder = `Extracting ${file.name}...`;
  try {
    target.value = await extractFile(file);
  } catch (error) {
    target.placeholder = error instanceof Error ? error.message : "Could not extract that file.";
  } finally {
    event.currentTarget.value = "";
  }
}

function jobLabel(job: IngestJob): string {
  if (job.status === "ready") {
    return "Plan generated. Review it and start when it feels right.";
  }
  if (job.status === "failed") {
    return `Could not generate this source${job.error_code ? `: ${job.error_code}` : "."}`;
  }
  return `Generating: ${job.status}`;
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
    case "profile":
      return "Profile";
  }
}

function returnActionStatus(action: "resume" | "restart" | "scale_down"): string {
  switch (action) {
    case "resume":
      return "Program resumed";
    case "restart":
      return "Program restarted";
    case "scale_down":
      return "Program scaled down";
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

function statusClass(status: string, score: number): string {
  if (status === "Complete") {
    return "perfect";
  }
  if (status === "Partial") {
    return "medium";
  }
  if (status === "Rest") {
    return "rest";
  }
  if (status === "Frozen") {
    return "frozen";
  }
  return heatClass(score);
}

function scoreFromDay(day: StatsResponse["days"][number]): number {
  if (day.available_points <= 0) {
    return 0;
  }
  return Math.round((day.earned_points / day.available_points) * 100);
}

function initials(name: string): string {
  return name
    .split(" ")
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0] ?? "")
    .join("")
    .toUpperCase() || "M";
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

function storedProfileName(): string {
  return localStorage.getItem("tracked.profileName") ?? "Dapper";
}

function saveProfileName(name: string): void {
  localStorage.setItem("tracked.profileName", name);
}

function storedTheme(): Theme {
  return localStorage.getItem("tracked.theme") === "light" ? "light" : "dark";
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
