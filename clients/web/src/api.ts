import type { Cadence } from "../../shared/src/types/Cadence";
import type { GeneratedProgram } from "../../shared/src/types/GeneratedProgram";
import type { Intensity } from "../../shared/src/types/Intensity";
import type { ProgramKind } from "../../shared/src/types/ProgramKind";

export type View =
  | "today"
  | "programs"
  | "ingest"
  | "standing"
  | "heatmap"
  | "cohort"
  | "profile";

export type SectionKind = "Program" | "Standing";

export type TodayResponse = {
  local_date: string;
  sections: TodaySection[];
  lapsed_days: number | null;
};

export type TodaySection = {
  enrollment_id: string;
  day_id: string;
  kind: SectionKind;
  title: string;
  day_index: number | null;
  duration_days: number | null;
  available_points: number;
  earned_points: number;
  note: string | null;
  streak: StreakSummary;
  tasks: TaskInstance[];
};

export type StreakSummary = {
  current: number;
  longest: number;
  freezes: number;
  state: string;
};

export type TaskInstance = {
  id: string;
  title: string;
  points: number;
  position: number;
  is_floating: boolean;
  completed_at: string | null;
  skipped_reason: string | null;
};

export type StandingTask = {
  id: string;
  title: string;
  position: number;
  estimated_minutes: number;
  cadence: Cadence;
  points: number;
};

export type IngestJob = {
  job_id: string;
  source_id: string | null;
  intensity: Intensity;
  status: "queued" | "normalising" | "classifying" | "generating" | "calibrating" | "ready" | "failed";
  instruction: string | null;
  draft: GeneratedProgram | null;
  warnings: unknown[];
  error_code: string | null;
  created_at: string;
};

export type Enrollment = {
  id: string;
  program_id: string;
  cohort_id: string | null;
  timezone: string;
  day_boundary_hour: number;
  start_date: string;
  is_standing: boolean;
  status: string;
  materialised_through: string | null;
};

export type StatsResponse = {
  days: Array<{
    id: string;
    local_date: string;
    status: string;
    available_points: number;
    earned_points: number;
    completed_tasks: number;
    available_tasks: number;
  }>;
  tasks: Array<{
    template_id: string;
    title: string;
    available_count: number;
    completed_count: number;
  }>;
};

export type NotificationEvent = {
  id: string;
  kind: string;
  scheduled_at: string;
  title: string;
  body: string;
  payload: unknown;
  status: string;
  skipped_reason: string | null;
  sent_at: string | null;
};

export type CompletionSummary = {
  id: string;
  enrollment_id: string;
  image_key: string | null;
  pdf_key: string | null;
  payload: {
    title: string;
    started_on: string;
    finished_on: string;
    days_total: number;
    days_logged: number;
    completion_rate: number | null;
    longest_streak: number;
    tasks_completed: number;
    hours_invested: number;
    cells: Array<{ local_date: string; status: string }>;
    notes: Array<{ local_date: string; note: string }>;
  };
};

export type Day = {
  id: string;
  enrollment_id: string;
  local_date: string;
  day_index: number;
  status: string;
  available_points: number;
  earned_points: number;
  note: string | null;
};

export type Cohort = {
  id: string;
  program_id: string;
  name: string | null;
  locked_start: string | null;
};

export type CohortPresence = {
  user_id: string;
  display_name: string | null;
  avatar_url: string | null;
  streak: number;
  logged_today: boolean;
};

export type ClientSettings = {
  apiBase: string;
  userId: string;
};

export type SessionResponse = {
  user_id: string;
  display_name: string;
  timezone: string;
};

export type ExtractResponse = {
  text: string;
  mime_type: string;
};

export class ApiClient {
  readonly settings: ClientSettings;

  constructor(settings: ClientSettings) {
    this.settings = settings;
  }

  createSession(timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || "Africa/Lagos", displayName = "Dapper"): Promise<SessionResponse> {
    return this.request<SessionResponse>("/v1/sessions", {
      method: "POST",
      body: { timezone, display_name: displayName },
      auth: false
    });
  }

  today(): Promise<TodayResponse> {
    return this.request<TodayResponse>("/v1/today");
  }

  completeTask(taskId: string): Promise<TaskInstance> {
    return this.request<TaskInstance>(`/v1/tasks/${taskId}/complete`, {
      method: "POST",
      body: { completed_at: new Date().toISOString() }
    });
  }

  uncompleteTask(taskId: string): Promise<TaskInstance> {
    return this.request<TaskInstance>(`/v1/tasks/${taskId}/complete`, { method: "DELETE" });
  }

  updateNote(dayId: string, note: string): Promise<unknown> {
    return this.request<unknown>(`/v1/days/${dayId}`, { method: "PATCH", body: { note } });
  }

  repairDay(dayId: string): Promise<unknown> {
    return this.request<unknown>(`/v1/days/${dayId}/repair`, { method: "POST" });
  }

  standing(): Promise<StandingTask[]> {
    return this.request<StandingTask[]>("/v1/standing");
  }

  createStanding(title: string, cadence: Cadence, durationBucket: "five" | "ten" | "fifteen" | "thirty"): Promise<StandingTask> {
    return this.request<StandingTask>("/v1/standing", {
      method: "POST",
      body: { title, cadence, duration_bucket: durationBucket }
    });
  }

  pauseStanding(id: string): Promise<StandingTask> {
    return this.request<StandingTask>(`/v1/standing/${id}/pause`, { method: "POST" });
  }

  enrollments(): Promise<Enrollment[]> {
    return this.request<Enrollment[]>("/v1/enrollments");
  }

  patchEnrollment(id: string, body: { status?: "active" | "paused" | "completed" | "abandoned"; timezone?: string; day_boundary_hour?: number }): Promise<Enrollment> {
    return this.request<Enrollment>(`/v1/enrollments/${id}`, {
      method: "PATCH",
      body
    });
  }

  returnEnrollment(id: string, action: "resume" | "restart" | "scale_down"): Promise<Enrollment> {
    return this.request<Enrollment>(`/v1/enrollments/${id}/return`, {
      method: "POST",
      body: { action }
    });
  }

  days(enrollment: string, from: string, to: string): Promise<Day[]> {
    return this.request<Day[]>(`/v1/days?enrollment=${enrollment}&from=${from}&to=${to}`);
  }

  stats(enrollment: string, from: string, to: string): Promise<StatsResponse> {
    return this.request<StatsResponse>(`/v1/stats?enrollment=${enrollment}&from=${from}&to=${to}`);
  }

  summary(enrollment: string): Promise<CompletionSummary> {
    return this.request<CompletionSummary>(`/v1/enrollments/${enrollment}/summary`);
  }

  notifications(): Promise<NotificationEvent[]> {
    return this.request<NotificationEvent[]>("/v1/notifications");
  }

  enqueueTestNotification(): Promise<NotificationEvent> {
    return this.request<NotificationEvent>("/v1/notifications/test", {
      method: "POST",
      body: { title: "Tracked", body: "Notifications are connected." }
    });
  }

  async extractFile(file: File): Promise<ExtractResponse> {
    return this.request<ExtractResponse>("/v1/extract", {
      method: "POST",
      body: {
        filename: file.name,
        mime_type: file.type || "application/octet-stream",
        data_base64: await fileBase64(file)
      }
    });
  }

  createIngest(sourceText: string, intensity: Intensity, instruction: string): Promise<{ job_id: string; status: string; cached: boolean }> {
    return this.request<{ job_id: string; status: string; cached: boolean }>("/v1/ingest", {
      method: "POST",
      body: {
        source_text: sourceText,
        mime_type: "text/plain",
        instruction: instruction.trim() || null,
        intensity
      }
    });
  }

  ingest(jobId: string): Promise<IngestJob> {
    return this.request<IngestJob>(`/v1/ingest/${jobId}`);
  }

  confirmDraft(jobId: string, startDate: string, timezone: string, dayBoundaryHour: number): Promise<unknown> {
    return this.request<unknown>("/v1/programs", {
      method: "POST",
      body: {
        ingest_job_id: jobId,
        start_date: startDate,
        timezone,
        day_boundary_hour: dayBoundaryHour
      }
    });
  }

  createCohort(programId: string, name: string): Promise<Cohort> {
    return this.request<Cohort>("/v1/cohorts", {
      method: "POST",
      body: {
        program_id: programId,
        name: name.trim() || null,
        locked_start: null
      }
    });
  }

  createInvite(cohortId: string): Promise<{ token: string }> {
    return this.request<{ token: string }>(`/v1/cohorts/${cohortId}/invites`, { method: "POST" });
  }

  joinCohort(token: string): Promise<{ cohort_id: string }> {
    return this.request<{ cohort_id: string }>("/v1/cohorts/join", {
      method: "POST",
      body: { token }
    });
  }

  cohortPresence(cohortId: string, localDate: string): Promise<CohortPresence[]> {
    return this.request<CohortPresence[]>(`/v1/cohorts/${cohortId}/presence?local_date=${localDate}`);
  }

  async request<T>(path: string, options: { method?: string; body?: unknown; auth?: boolean } = {}): Promise<T> {
    const needsAuth = options.auth !== false;
    if (needsAuth && !this.settings.userId.trim()) {
      throw new Error("Session is not ready yet. Refresh and try again.");
    }

    const headers = new Headers();
    headers.set("content-type", "application/json");
    if (needsAuth) {
      headers.set("x-user-id", this.settings.userId);
    }

    const init: RequestInit = {
      method: options.method ?? "GET",
      headers
    };
    if (options.body !== undefined) {
      init.body = JSON.stringify(options.body);
    }

    const response = await fetch(`${this.settings.apiBase}${path}`, init);

    const text = await response.text();
    const payload: unknown = text ? JSON.parse(text) : null;

    if (!response.ok) {
      const message = isErrorPayload(payload) ? payload.error : response.statusText;
      throw new Error(message);
    }

    return payload as T;
  }
}

async function fileBase64(file: File): Promise<string> {
  const buffer = await file.arrayBuffer();
  const bytes = new Uint8Array(buffer);
  let binary = "";
  const chunkSize = 0x8000;
  for (let index = 0; index < bytes.length; index += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunkSize));
  }
  return window.btoa(binary);
}

export const defaultSettings: ClientSettings = {
  apiBase: storedApiBase(),
  userId: localStorage.getItem("tracked.userId") ?? ""
};

export function saveSettings(settings: ClientSettings): void {
  if (settings.apiBase.trim()) {
    localStorage.setItem("tracked.apiBase", settings.apiBase);
  } else {
    localStorage.removeItem("tracked.apiBase");
  }
  localStorage.setItem("tracked.userId", settings.userId);
}

export function clearSessionSettings(): void {
  localStorage.removeItem("tracked.userId");
  localStorage.removeItem("tracked.apiBase");
}

function storedApiBase(): string {
  const value = localStorage.getItem("tracked.apiBase") ?? "";
  if (value === "http://localhost:8080" || value === "http://127.0.0.1:8080") {
    localStorage.removeItem("tracked.apiBase");
    return "";
  }
  return value;
}

function isErrorPayload(value: unknown): value is { error: string } {
  return typeof value === "object" && value !== null && "error" in value;
}

export function dayScore(section: TodaySection): number {
  if (section.available_points <= 0) {
    return 0;
  }
  return Math.round((section.earned_points / section.available_points) * 100);
}

export function programKindLabel(kind: ProgramKind): string {
  switch (kind) {
    case "curriculum":
      return "Curriculum";
    case "routine":
      return "Routine";
    case "project":
      return "Project";
  }
}
