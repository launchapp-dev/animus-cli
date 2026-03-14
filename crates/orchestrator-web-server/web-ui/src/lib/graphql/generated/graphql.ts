/* eslint-disable */
import { DocumentTypeDecoration } from '@graphql-typed-document-node/core';
export type Maybe<T> = T | null;
export type InputMaybe<T> = T | null | undefined;
export type Exact<T extends { [key: string]: unknown }> = { [K in keyof T]: T[K] };
export type MakeOptional<T, K extends keyof T> = Omit<T, K> & { [SubKey in K]?: Maybe<T[SubKey]> };
export type MakeMaybe<T, K extends keyof T> = Omit<T, K> & { [SubKey in K]: Maybe<T[SubKey]> };
export type MakeEmpty<T extends { [key: string]: unknown }, K extends keyof T> = { [_ in K]?: never };
export type Incremental<T> = T | { [P in keyof T]?: P extends ' $fragmentName' | '__typename' ? T[P] : never };
/** All built-in and custom scalars, mapped to their actual values */
export type Scalars = {
  ID: { input: string; output: string; }
  String: { input: string; output: string; }
  Boolean: { input: boolean; output: boolean; }
  Int: { input: number; output: number; }
  Float: { input: number; output: number; }
};

export type GqlAgentProfile = {
  __typename?: 'GqlAgentProfile';
  description: Scalars['String']['output'];
  mcpServers: Array<Scalars['String']['output']>;
  model?: Maybe<Scalars['String']['output']>;
  name: Scalars['String']['output'];
  role?: Maybe<Scalars['String']['output']>;
  skills: Array<Scalars['String']['output']>;
  tool?: Maybe<Scalars['String']['output']>;
};

export type GqlAgentRun = {
  __typename?: 'GqlAgentRun';
  phaseId?: Maybe<Scalars['String']['output']>;
  runId: Scalars['String']['output'];
  status: Scalars['String']['output'];
  taskId?: Maybe<Scalars['String']['output']>;
  taskTitle?: Maybe<Scalars['String']['output']>;
  workflowId?: Maybe<Scalars['String']['output']>;
};

export type GqlChecklist = {
  __typename?: 'GqlChecklist';
  completed: Scalars['Boolean']['output'];
  description: Scalars['String']['output'];
  id: Scalars['String']['output'];
};

export enum GqlComplexity {
  High = 'HIGH',
  Low = 'LOW',
  Medium = 'MEDIUM'
}

export type GqlDaemonEvent = {
  __typename?: 'GqlDaemonEvent';
  data: Scalars['String']['output'];
  eventType: Scalars['String']['output'];
  id: Scalars['String']['output'];
  seq: Scalars['Int']['output'];
  timestamp: Scalars['String']['output'];
};

export type GqlDaemonHealth = {
  __typename?: 'GqlDaemonHealth';
  activeAgents: Scalars['Int']['output'];
  daemonPid?: Maybe<Scalars['Int']['output']>;
  healthy: Scalars['Boolean']['output'];
  runnerConnected: Scalars['Boolean']['output'];
  runnerPid?: Maybe<Scalars['Int']['output']>;
  status: Scalars['String']['output'];
};

export type GqlDaemonLog = {
  __typename?: 'GqlDaemonLog';
  fields?: Maybe<Scalars['String']['output']>;
  level?: Maybe<Scalars['String']['output']>;
  message?: Maybe<Scalars['String']['output']>;
  timestamp?: Maybe<Scalars['String']['output']>;
};

export type GqlDaemonStatus = {
  __typename?: 'GqlDaemonStatus';
  activeAgents: Scalars['Int']['output'];
  healthy: Scalars['Boolean']['output'];
  maxAgents?: Maybe<Scalars['Int']['output']>;
  projectRoot?: Maybe<Scalars['String']['output']>;
  runnerConnected: Scalars['Boolean']['output'];
  status: GqlDaemonStatusValue;
  statusRaw?: Maybe<Scalars['String']['output']>;
};

export enum GqlDaemonStatusValue {
  Crashed = 'CRASHED',
  Paused = 'PAUSED',
  Running = 'RUNNING',
  Starting = 'STARTING',
  Stopped = 'STOPPED',
  Stopping = 'STOPPING'
}

export type GqlDecision = {
  __typename?: 'GqlDecision';
  confidence: Scalars['Float']['output'];
  decision: Scalars['String']['output'];
  phaseId: Scalars['String']['output'];
  reason: Scalars['String']['output'];
  risk: Scalars['String']['output'];
  source: Scalars['String']['output'];
  targetPhase?: Maybe<Scalars['String']['output']>;
  timestamp: Scalars['String']['output'];
};

export type GqlDependency = {
  __typename?: 'GqlDependency';
  taskId: Scalars['String']['output'];
  type: Scalars['String']['output'];
};

export type GqlKeyValue = {
  __typename?: 'GqlKeyValue';
  key: Scalars['String']['output'];
  value: Scalars['String']['output'];
};

export type GqlMcpServer = {
  __typename?: 'GqlMcpServer';
  args: Array<Scalars['String']['output']>;
  command: Scalars['String']['output'];
  env: Array<GqlKeyValue>;
  name: Scalars['String']['output'];
  tools: Array<Scalars['String']['output']>;
  transport?: Maybe<Scalars['String']['output']>;
};

export type GqlPhaseCatalogEntry = {
  __typename?: 'GqlPhaseCatalogEntry';
  category: Scalars['String']['output'];
  description: Scalars['String']['output'];
  id: Scalars['String']['output'];
  label: Scalars['String']['output'];
  tags: Array<Scalars['String']['output']>;
};

export type GqlPhaseExecution = {
  __typename?: 'GqlPhaseExecution';
  attempt: Scalars['Int']['output'];
  completedAt?: Maybe<Scalars['String']['output']>;
  errorMessage?: Maybe<Scalars['String']['output']>;
  phaseId: Scalars['String']['output'];
  startedAt?: Maybe<Scalars['String']['output']>;
  status: Scalars['String']['output'];
};

export type GqlPhaseOutput = {
  __typename?: 'GqlPhaseOutput';
  hasMore: Scalars['Boolean']['output'];
  lines: Array<Scalars['String']['output']>;
  phaseId: Scalars['String']['output'];
};

export enum GqlPriority {
  Critical = 'CRITICAL',
  High = 'HIGH',
  Low = 'LOW',
  Medium = 'MEDIUM'
}

export type GqlProject = {
  __typename?: 'GqlProject';
  archived: Scalars['Boolean']['output'];
  description?: Maybe<Scalars['String']['output']>;
  id: Scalars['ID']['output'];
  metadata?: Maybe<Scalars['String']['output']>;
  name?: Maybe<Scalars['String']['output']>;
  path?: Maybe<Scalars['String']['output']>;
  requirements: Array<GqlRequirement>;
  tasks: Array<GqlTask>;
  techStack: Array<Scalars['String']['output']>;
  type?: Maybe<Scalars['String']['output']>;
  workflows: Array<GqlWorkflow>;
};

export type GqlQueueEntry = {
  __typename?: 'GqlQueueEntry';
  position?: Maybe<Scalars['Int']['output']>;
  priority?: Maybe<GqlPriority>;
  status?: Maybe<GqlTaskStatus>;
  taskId: Scalars['String']['output'];
  title?: Maybe<Scalars['String']['output']>;
  waitTime?: Maybe<Scalars['Float']['output']>;
};

export type GqlQueueStats = {
  __typename?: 'GqlQueueStats';
  avgWait?: Maybe<Scalars['Float']['output']>;
  depth: Scalars['Int']['output'];
  heldCount: Scalars['Int']['output'];
  readyCount: Scalars['Int']['output'];
  throughput?: Maybe<Scalars['Float']['output']>;
};

export type GqlRequirement = {
  __typename?: 'GqlRequirement';
  acceptanceCriteria: Array<Scalars['String']['output']>;
  description: Scalars['String']['output'];
  id: Scalars['ID']['output'];
  linkedTaskIds: Array<Scalars['String']['output']>;
  priority: GqlRequirementPriority;
  priorityRaw: Scalars['String']['output'];
  requirementType?: Maybe<GqlRequirementType>;
  status: GqlRequirementStatus;
  statusRaw: Scalars['String']['output'];
  tags: Array<Scalars['String']['output']>;
  title: Scalars['String']['output'];
};

export type GqlRequirementConnection = {
  __typename?: 'GqlRequirementConnection';
  items: Array<GqlRequirement>;
  totalCount: Scalars['Int']['output'];
};

export enum GqlRequirementPriority {
  Could = 'COULD',
  Must = 'MUST',
  Should = 'SHOULD',
  Wont = 'WONT'
}

export enum GqlRequirementStatus {
  Approved = 'APPROVED',
  Deprecated = 'DEPRECATED',
  Done = 'DONE',
  Draft = 'DRAFT',
  EmReview = 'EM_REVIEW',
  Implemented = 'IMPLEMENTED',
  InProgress = 'IN_PROGRESS',
  NeedsRework = 'NEEDS_REWORK',
  Planned = 'PLANNED',
  PoReview = 'PO_REVIEW',
  Refined = 'REFINED'
}

export enum GqlRequirementType {
  Functional = 'FUNCTIONAL',
  NonFunctional = 'NON_FUNCTIONAL',
  Other = 'OTHER',
  Product = 'PRODUCT',
  Technical = 'TECHNICAL'
}

export enum GqlRiskLevel {
  High = 'HIGH',
  Low = 'LOW',
  Medium = 'MEDIUM'
}

export enum GqlScope {
  Large = 'LARGE',
  Medium = 'MEDIUM',
  Small = 'SMALL'
}

export type GqlSkill = {
  __typename?: 'GqlSkill';
  category: Scalars['String']['output'];
  description: Scalars['String']['output'];
  name: Scalars['String']['output'];
  skillType: Scalars['String']['output'];
  source: Scalars['String']['output'];
};

export type GqlSkillDetail = {
  __typename?: 'GqlSkillDetail';
  category: Scalars['String']['output'];
  definitionJson: Scalars['String']['output'];
  description: Scalars['String']['output'];
  name: Scalars['String']['output'];
  skillType: Scalars['String']['output'];
  source: Scalars['String']['output'];
};

export type GqlSystemInfo = {
  __typename?: 'GqlSystemInfo';
  arch?: Maybe<Scalars['String']['output']>;
  daemonStatus?: Maybe<Scalars['String']['output']>;
  platform?: Maybe<Scalars['String']['output']>;
  projectRoot?: Maybe<Scalars['String']['output']>;
  version?: Maybe<Scalars['String']['output']>;
};

export type GqlTask = {
  __typename?: 'GqlTask';
  checklist: Array<GqlChecklist>;
  complexity: GqlComplexity;
  dependencies: Array<GqlDependency>;
  description: Scalars['String']['output'];
  id: Scalars['ID']['output'];
  linkedRequirementIds: Array<Scalars['String']['output']>;
  priority: GqlPriority;
  priorityRaw: Scalars['String']['output'];
  requirements: Array<GqlRequirement>;
  risk: GqlRiskLevel;
  scope: GqlScope;
  status: GqlTaskStatus;
  statusRaw: Scalars['String']['output'];
  tags: Array<Scalars['String']['output']>;
  taskType: GqlTaskType;
  taskTypeRaw: Scalars['String']['output'];
  title: Scalars['String']['output'];
};

export type GqlTaskConnection = {
  __typename?: 'GqlTaskConnection';
  items: Array<GqlTask>;
  totalCount: Scalars['Int']['output'];
};

export type GqlTaskStats = {
  __typename?: 'GqlTaskStats';
  byPriority?: Maybe<Scalars['String']['output']>;
  byStatus?: Maybe<Scalars['String']['output']>;
  byType?: Maybe<Scalars['String']['output']>;
  raw: Scalars['String']['output'];
  total: Scalars['Int']['output'];
};

export enum GqlTaskStatus {
  Backlog = 'BACKLOG',
  Blocked = 'BLOCKED',
  Cancelled = 'CANCELLED',
  Done = 'DONE',
  InProgress = 'IN_PROGRESS',
  OnHold = 'ON_HOLD',
  Ready = 'READY'
}

export enum GqlTaskType {
  Bugfix = 'BUGFIX',
  Chore = 'CHORE',
  Docs = 'DOCS',
  Experiment = 'EXPERIMENT',
  Feature = 'FEATURE',
  Hotfix = 'HOTFIX',
  Refactor = 'REFACTOR',
  Test = 'TEST'
}

export type GqlToolDefinition = {
  __typename?: 'GqlToolDefinition';
  contextWindow?: Maybe<Scalars['Int']['output']>;
  executable: Scalars['String']['output'];
  name: Scalars['String']['output'];
  supportsMcp: Scalars['Boolean']['output'];
  supportsWrite: Scalars['Boolean']['output'];
};

export type GqlVision = {
  __typename?: 'GqlVision';
  constraints: Array<Scalars['String']['output']>;
  goals: Array<Scalars['String']['output']>;
  raw: Scalars['String']['output'];
  successCriteria: Array<Scalars['String']['output']>;
  summary?: Maybe<Scalars['String']['output']>;
  targetAudience?: Maybe<Scalars['String']['output']>;
  title?: Maybe<Scalars['String']['output']>;
};

export type GqlWorkflow = {
  __typename?: 'GqlWorkflow';
  currentPhase?: Maybe<Scalars['String']['output']>;
  decisions: Array<GqlDecision>;
  id: Scalars['ID']['output'];
  phases: Array<GqlPhaseExecution>;
  status: GqlWorkflowStatus;
  statusRaw: Scalars['String']['output'];
  taskId: Scalars['String']['output'];
  totalReworks: Scalars['Int']['output'];
  workflowRef?: Maybe<Scalars['String']['output']>;
};

export type GqlWorkflowCheckpoint = {
  __typename?: 'GqlWorkflowCheckpoint';
  data?: Maybe<Scalars['String']['output']>;
  id: Scalars['String']['output'];
  phase: Scalars['String']['output'];
  timestamp?: Maybe<Scalars['String']['output']>;
};

export type GqlWorkflowConfig = {
  __typename?: 'GqlWorkflowConfig';
  agentProfiles: Array<GqlAgentProfile>;
  mcpServers: Array<GqlMcpServer>;
  phaseCatalog: Array<GqlPhaseCatalogEntry>;
  schedules: Array<GqlWorkflowSchedule>;
  tools: Array<GqlToolDefinition>;
};

export type GqlWorkflowConnection = {
  __typename?: 'GqlWorkflowConnection';
  items: Array<GqlWorkflow>;
  totalCount: Scalars['Int']['output'];
};

export type GqlWorkflowDefinition = {
  __typename?: 'GqlWorkflowDefinition';
  description?: Maybe<Scalars['String']['output']>;
  id: Scalars['String']['output'];
  name: Scalars['String']['output'];
  phases: Array<Scalars['String']['output']>;
};

export type GqlWorkflowSchedule = {
  __typename?: 'GqlWorkflowSchedule';
  command?: Maybe<Scalars['String']['output']>;
  cron: Scalars['String']['output'];
  enabled: Scalars['Boolean']['output'];
  id: Scalars['String']['output'];
  workflowRef?: Maybe<Scalars['String']['output']>;
};

export enum GqlWorkflowStatus {
  Cancelled = 'CANCELLED',
  Completed = 'COMPLETED',
  Escalated = 'ESCALATED',
  Failed = 'FAILED',
  Paused = 'PAUSED',
  Pending = 'PENDING',
  Running = 'RUNNING'
}

export type MutationRoot = {
  __typename?: 'MutationRoot';
  approvePhase: GqlWorkflow;
  archiveProject: GqlProject;
  assignAgent: GqlTask;
  assignHuman: GqlTask;
  cancelWorkflow: GqlWorkflow;
  checklistAdd: GqlTask;
  checklistUpdate: GqlTask;
  createProject: GqlProject;
  createRequirement: GqlRequirement;
  createTask: GqlTask;
  daemonClearLogs: Scalars['Boolean']['output'];
  daemonPause: Scalars['Boolean']['output'];
  daemonResume: Scalars['Boolean']['output'];
  daemonStart: Scalars['Boolean']['output'];
  daemonStop: Scalars['Boolean']['output'];
  deleteProject: Scalars['Boolean']['output'];
  deleteRequirement: Scalars['Boolean']['output'];
  deleteTask: Scalars['Boolean']['output'];
  deleteWorkflowDefinition: Scalars['Boolean']['output'];
  dependencyAdd: GqlTask;
  dependencyRemove: GqlTask;
  draftRequirement: GqlRequirement;
  loadProject: GqlProject;
  pauseWorkflow: GqlWorkflow;
  queueHold: Scalars['Boolean']['output'];
  queueRelease: Scalars['Boolean']['output'];
  queueReorder: Scalars['Boolean']['output'];
  refineRequirement: GqlRequirement;
  refineVision: GqlVision;
  resumeWorkflow: GqlWorkflow;
  reviewHandoff: Scalars['Boolean']['output'];
  runWorkflow: GqlWorkflow;
  saveVision: GqlVision;
  saveWorkflowConfig: Scalars['Boolean']['output'];
  updateProject: GqlProject;
  updateRequirement: GqlRequirement;
  updateTask: GqlTask;
  updateTaskStatus: GqlTask;
  upsertWorkflowDefinition: Scalars['Boolean']['output'];
};


export type MutationRootApprovePhaseArgs = {
  note?: InputMaybe<Scalars['String']['input']>;
  phaseId: Scalars['String']['input'];
  workflowId: Scalars['ID']['input'];
};


export type MutationRootArchiveProjectArgs = {
  id: Scalars['ID']['input'];
};


export type MutationRootAssignAgentArgs = {
  id: Scalars['ID']['input'];
  model?: InputMaybe<Scalars['String']['input']>;
  role?: InputMaybe<Scalars['String']['input']>;
};


export type MutationRootAssignHumanArgs = {
  id: Scalars['ID']['input'];
  name: Scalars['String']['input'];
};


export type MutationRootCancelWorkflowArgs = {
  id: Scalars['ID']['input'];
};


export type MutationRootChecklistAddArgs = {
  description: Scalars['String']['input'];
  id: Scalars['ID']['input'];
};


export type MutationRootChecklistUpdateArgs = {
  completed?: InputMaybe<Scalars['Boolean']['input']>;
  description?: InputMaybe<Scalars['String']['input']>;
  id: Scalars['ID']['input'];
  itemId: Scalars['String']['input'];
};


export type MutationRootCreateProjectArgs = {
  description?: InputMaybe<Scalars['String']['input']>;
  name: Scalars['String']['input'];
  path: Scalars['String']['input'];
  projectType?: InputMaybe<Scalars['String']['input']>;
};


export type MutationRootCreateRequirementArgs = {
  acceptanceCriteria?: InputMaybe<Array<Scalars['String']['input']>>;
  description?: InputMaybe<Scalars['String']['input']>;
  priority?: InputMaybe<Scalars['String']['input']>;
  requirementType?: InputMaybe<Scalars['String']['input']>;
  title: Scalars['String']['input'];
};


export type MutationRootCreateTaskArgs = {
  description?: InputMaybe<Scalars['String']['input']>;
  priority?: InputMaybe<Scalars['String']['input']>;
  taskType?: InputMaybe<Scalars['String']['input']>;
  title: Scalars['String']['input'];
};


export type MutationRootDeleteProjectArgs = {
  id: Scalars['ID']['input'];
};


export type MutationRootDeleteRequirementArgs = {
  id: Scalars['ID']['input'];
};


export type MutationRootDeleteTaskArgs = {
  id: Scalars['ID']['input'];
};


export type MutationRootDeleteWorkflowDefinitionArgs = {
  id: Scalars['ID']['input'];
};


export type MutationRootDependencyAddArgs = {
  dependencyType?: InputMaybe<Scalars['String']['input']>;
  dependsOn: Scalars['String']['input'];
  id: Scalars['ID']['input'];
};


export type MutationRootDependencyRemoveArgs = {
  dependsOn: Scalars['String']['input'];
  id: Scalars['ID']['input'];
};


export type MutationRootDraftRequirementArgs = {
  context?: InputMaybe<Scalars['String']['input']>;
};


export type MutationRootLoadProjectArgs = {
  id: Scalars['ID']['input'];
};


export type MutationRootPauseWorkflowArgs = {
  id: Scalars['ID']['input'];
};


export type MutationRootQueueHoldArgs = {
  reason?: InputMaybe<Scalars['String']['input']>;
  taskId: Scalars['String']['input'];
};


export type MutationRootQueueReleaseArgs = {
  taskId: Scalars['String']['input'];
};


export type MutationRootQueueReorderArgs = {
  taskIds: Array<Scalars['String']['input']>;
};


export type MutationRootRefineRequirementArgs = {
  feedback?: InputMaybe<Scalars['String']['input']>;
  id: Scalars['String']['input'];
};


export type MutationRootRefineVisionArgs = {
  feedback?: InputMaybe<Scalars['String']['input']>;
};


export type MutationRootResumeWorkflowArgs = {
  feedback?: InputMaybe<Scalars['String']['input']>;
  id: Scalars['ID']['input'];
};


export type MutationRootReviewHandoffArgs = {
  context?: InputMaybe<Scalars['String']['input']>;
  question: Scalars['String']['input'];
  targetRole: Scalars['String']['input'];
};


export type MutationRootRunWorkflowArgs = {
  taskId: Scalars['String']['input'];
  workflowRef?: InputMaybe<Scalars['String']['input']>;
};


export type MutationRootSaveVisionArgs = {
  content: Scalars['String']['input'];
};


export type MutationRootSaveWorkflowConfigArgs = {
  configJson: Scalars['String']['input'];
};


export type MutationRootUpdateProjectArgs = {
  description?: InputMaybe<Scalars['String']['input']>;
  id: Scalars['ID']['input'];
  name?: InputMaybe<Scalars['String']['input']>;
  projectType?: InputMaybe<Scalars['String']['input']>;
};


export type MutationRootUpdateRequirementArgs = {
  acceptanceCriteria?: InputMaybe<Array<Scalars['String']['input']>>;
  description?: InputMaybe<Scalars['String']['input']>;
  id: Scalars['ID']['input'];
  priority?: InputMaybe<Scalars['String']['input']>;
  requirementType?: InputMaybe<Scalars['String']['input']>;
  status?: InputMaybe<Scalars['String']['input']>;
  title?: InputMaybe<Scalars['String']['input']>;
};


export type MutationRootUpdateTaskArgs = {
  complexity?: InputMaybe<Scalars['String']['input']>;
  description?: InputMaybe<Scalars['String']['input']>;
  id: Scalars['ID']['input'];
  priority?: InputMaybe<Scalars['String']['input']>;
  risk?: InputMaybe<Scalars['String']['input']>;
  scope?: InputMaybe<Scalars['String']['input']>;
  taskType?: InputMaybe<Scalars['String']['input']>;
  title?: InputMaybe<Scalars['String']['input']>;
};


export type MutationRootUpdateTaskStatusArgs = {
  id: Scalars['ID']['input'];
  status: Scalars['String']['input'];
};


export type MutationRootUpsertWorkflowDefinitionArgs = {
  description?: InputMaybe<Scalars['String']['input']>;
  id: Scalars['String']['input'];
  name: Scalars['String']['input'];
  phases: Scalars['String']['input'];
  variables?: InputMaybe<Scalars['String']['input']>;
};

export type QueryRoot = {
  __typename?: 'QueryRoot';
  agentRuns: Array<GqlAgentRun>;
  daemonHealth: GqlDaemonHealth;
  daemonLogs: Array<GqlDaemonLog>;
  daemonStatus: GqlDaemonStatus;
  phaseOutput: GqlPhaseOutput;
  project?: Maybe<GqlProject>;
  projects: Array<GqlProject>;
  projectsActive: Array<GqlProject>;
  queue: Array<GqlQueueEntry>;
  queueStats: GqlQueueStats;
  readyTasks: Array<GqlTask>;
  requirement?: Maybe<GqlRequirement>;
  requirements: Array<GqlRequirement>;
  requirementsPaginated: GqlRequirementConnection;
  skillDetail?: Maybe<GqlSkillDetail>;
  skills: Array<GqlSkill>;
  systemInfo: GqlSystemInfo;
  task?: Maybe<GqlTask>;
  taskStats: GqlTaskStats;
  tasks: Array<GqlTask>;
  tasksNext?: Maybe<GqlTask>;
  tasksPaginated: GqlTaskConnection;
  tasksPrioritized: Array<GqlTask>;
  vision?: Maybe<GqlVision>;
  workflow?: Maybe<GqlWorkflow>;
  workflowCheckpoints: Array<GqlWorkflowCheckpoint>;
  workflowConfig: GqlWorkflowConfig;
  workflowDefinitions: Array<GqlWorkflowDefinition>;
  workflows: Array<GqlWorkflow>;
  workflowsPaginated: GqlWorkflowConnection;
};


export type QueryRootDaemonLogsArgs = {
  limit?: InputMaybe<Scalars['Int']['input']>;
};


export type QueryRootPhaseOutputArgs = {
  phaseId?: InputMaybe<Scalars['String']['input']>;
  tail?: InputMaybe<Scalars['Int']['input']>;
  workflowId: Scalars['ID']['input'];
};


export type QueryRootProjectArgs = {
  id: Scalars['ID']['input'];
};


export type QueryRootReadyTasksArgs = {
  limit?: InputMaybe<Scalars['Int']['input']>;
  search?: InputMaybe<Scalars['String']['input']>;
};


export type QueryRootRequirementArgs = {
  id: Scalars['ID']['input'];
};


export type QueryRootRequirementsPaginatedArgs = {
  limit?: Scalars['Int']['input'];
  offset?: Scalars['Int']['input'];
};


export type QueryRootSkillDetailArgs = {
  name: Scalars['String']['input'];
};


export type QueryRootTaskArgs = {
  id: Scalars['ID']['input'];
};


export type QueryRootTasksArgs = {
  priority?: InputMaybe<Scalars['String']['input']>;
  search?: InputMaybe<Scalars['String']['input']>;
  status?: InputMaybe<Scalars['String']['input']>;
  taskType?: InputMaybe<Scalars['String']['input']>;
};


export type QueryRootTasksPaginatedArgs = {
  limit?: Scalars['Int']['input'];
  offset?: Scalars['Int']['input'];
  priority?: InputMaybe<Scalars['String']['input']>;
  search?: InputMaybe<Scalars['String']['input']>;
  status?: InputMaybe<Scalars['String']['input']>;
  taskType?: InputMaybe<Scalars['String']['input']>;
};


export type QueryRootWorkflowArgs = {
  id: Scalars['ID']['input'];
};


export type QueryRootWorkflowCheckpointsArgs = {
  workflowId: Scalars['ID']['input'];
};


export type QueryRootWorkflowsArgs = {
  status?: InputMaybe<Scalars['String']['input']>;
};


export type QueryRootWorkflowsPaginatedArgs = {
  limit?: Scalars['Int']['input'];
  offset?: Scalars['Int']['input'];
  status?: InputMaybe<Scalars['String']['input']>;
};

export type SubscriptionRoot = {
  __typename?: 'SubscriptionRoot';
  daemonEvents: GqlDaemonEvent;
  taskEvents: GqlDaemonEvent;
  workflowEvents: GqlDaemonEvent;
};


export type SubscriptionRootDaemonEventsArgs = {
  eventType?: InputMaybe<Scalars['String']['input']>;
};


export type SubscriptionRootTaskEventsArgs = {
  taskId?: InputMaybe<Scalars['String']['input']>;
};


export type SubscriptionRootWorkflowEventsArgs = {
  workflowId?: InputMaybe<Scalars['String']['input']>;
};

export type UpsertWorkflowDefinitionMutationVariables = Exact<{
  id: Scalars['String']['input'];
  name: Scalars['String']['input'];
  description?: InputMaybe<Scalars['String']['input']>;
  phases: Scalars['String']['input'];
  variables?: InputMaybe<Scalars['String']['input']>;
}>;


export type UpsertWorkflowDefinitionMutation = { __typename?: 'MutationRoot', upsertWorkflowDefinition: boolean };

export type DeleteWorkflowDefinitionMutationVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type DeleteWorkflowDefinitionMutation = { __typename?: 'MutationRoot', deleteWorkflowDefinition: boolean };

export type DaemonQueryVariables = Exact<{ [key: string]: never; }>;


export type DaemonQuery = { __typename?: 'QueryRoot', daemonStatus: { __typename?: 'GqlDaemonStatus', healthy: boolean, status: GqlDaemonStatusValue, statusRaw?: string | null, runnerConnected: boolean, activeAgents: number, maxAgents?: number | null, projectRoot?: string | null }, daemonHealth: { __typename?: 'GqlDaemonHealth', healthy: boolean, status: string, runnerConnected: boolean, runnerPid?: number | null, activeAgents: number, daemonPid?: number | null }, agentRuns: Array<{ __typename?: 'GqlAgentRun', runId: string, taskId?: string | null, taskTitle?: string | null, workflowId?: string | null, phaseId?: string | null, status: string }>, daemonLogs: Array<{ __typename?: 'GqlDaemonLog', timestamp?: string | null, level?: string | null, message?: string | null }> };

export type DaemonStartMutationVariables = Exact<{ [key: string]: never; }>;


export type DaemonStartMutation = { __typename?: 'MutationRoot', daemonStart: boolean };

export type DaemonStopMutationVariables = Exact<{ [key: string]: never; }>;


export type DaemonStopMutation = { __typename?: 'MutationRoot', daemonStop: boolean };

export type DaemonPauseMutationVariables = Exact<{ [key: string]: never; }>;


export type DaemonPauseMutation = { __typename?: 'MutationRoot', daemonPause: boolean };

export type DaemonResumeMutationVariables = Exact<{ [key: string]: never; }>;


export type DaemonResumeMutation = { __typename?: 'MutationRoot', daemonResume: boolean };

export type DaemonClearLogsMutationVariables = Exact<{ [key: string]: never; }>;


export type DaemonClearLogsMutation = { __typename?: 'MutationRoot', daemonClearLogs: boolean };

export type DashboardQueryVariables = Exact<{ [key: string]: never; }>;


export type DashboardQuery = { __typename?: 'QueryRoot', taskStats: { __typename?: 'GqlTaskStats', total: number, byStatus?: string | null, byPriority?: string | null }, daemonHealth: { __typename?: 'GqlDaemonHealth', healthy: boolean, status: string, runnerConnected: boolean, daemonPid?: number | null, activeDaemons: number }, agentRuns: Array<{ __typename?: 'GqlAgentRun', runId: string, taskId?: string | null, taskTitle?: string | null, workflowId?: string | null, phaseId?: string | null, status: string }>, systemInfo: { __typename?: 'GqlSystemInfo', platform?: string | null, version?: string | null, daemonStatus?: string | null, projectRoot?: string | null }, queueStats: { __typename?: 'GqlQueueStats', depth: number } };

export type ReadyTasksQueryVariables = Exact<{
  search?: InputMaybe<Scalars['String']['input']>;
  limit?: InputMaybe<Scalars['Int']['input']>;
}>;


export type ReadyTasksQuery = { __typename?: 'QueryRoot', readyTasks: Array<{ __typename?: 'GqlTask', id: string, title: string, statusRaw: string, priorityRaw: string, taskTypeRaw: string }> };

export type WorkflowDefinitionsQueryVariables = Exact<{ [key: string]: never; }>;


export type WorkflowDefinitionsQuery = { __typename?: 'QueryRoot', workflowDefinitions: Array<{ __typename?: 'GqlWorkflowDefinition', id: string, name: string, description?: string | null, phases: Array<string> }> };

export type DispatchRequirementsQueryVariables = Exact<{ [key: string]: never; }>;


export type DispatchRequirementsQuery = { __typename?: 'QueryRoot', requirements: Array<{ __typename?: 'GqlRequirement', id: string, title: string, description: string, priorityRaw: string, statusRaw: string, requirementType?: GqlRequirementType | null, tags: Array<string>, linkedTaskIds: Array<string>, acceptanceCriteria: Array<string> }> };

export type DaemonEventsSubscriptionVariables = Exact<{
  eventType?: InputMaybe<Scalars['String']['input']>;
}>;


export type DaemonEventsSubscription = { __typename?: 'SubscriptionRoot', daemonEvents: { __typename?: 'GqlDaemonEvent', id: string, seq: number, timestamp: string, eventType: string, data: string } };

export type VisionQueryVariables = Exact<{ [key: string]: never; }>;


export type VisionQuery = { __typename?: 'QueryRoot', vision?: { __typename?: 'GqlVision', title?: string | null, summary?: string | null, goals: Array<string>, targetAudience?: string | null, successCriteria: Array<string>, constraints: Array<string>, raw: string } | null };

export type RequirementsQueryVariables = Exact<{ [key: string]: never; }>;


export type RequirementsQuery = { __typename?: 'QueryRoot', requirements: Array<{ __typename?: 'GqlRequirement', id: string, title: string, description: string, priority: GqlRequirementPriority, priorityRaw: string, status: GqlRequirementStatus, statusRaw: string, requirementType?: GqlRequirementType | null, tags: Array<string>, linkedTaskIds: Array<string>, acceptanceCriteria: Array<string> }> };

export type RequirementQueryVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type RequirementQuery = { __typename?: 'QueryRoot', requirement?: { __typename?: 'GqlRequirement', id: string, title: string, description: string, priority: GqlRequirementPriority, priorityRaw: string, status: GqlRequirementStatus, statusRaw: string, requirementType?: GqlRequirementType | null, tags: Array<string>, linkedTaskIds: Array<string>, acceptanceCriteria: Array<string> } | null };

export type RequirementDetailQueryVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type RequirementDetailQuery = { __typename?: 'QueryRoot', requirement?: { __typename?: 'GqlRequirement', id: string, title: string, description: string, priority: GqlRequirementPriority, priorityRaw: string, status: GqlRequirementStatus, statusRaw: string, requirementType?: GqlRequirementType | null, tags: Array<string>, linkedTaskIds: Array<string>, acceptanceCriteria: Array<string> } | null };

export type SaveVisionMutationVariables = Exact<{
  content: Scalars['String']['input'];
}>;


export type SaveVisionMutation = { __typename?: 'MutationRoot', saveVision: { __typename?: 'GqlVision', title?: string | null, summary?: string | null, goals: Array<string>, targetAudience?: string | null, successCriteria: Array<string>, constraints: Array<string>, raw: string } };

export type RefineVisionMutationVariables = Exact<{
  feedback?: InputMaybe<Scalars['String']['input']>;
}>;


export type RefineVisionMutation = { __typename?: 'MutationRoot', refineVision: { __typename?: 'GqlVision', title?: string | null, summary?: string | null, goals: Array<string>, targetAudience?: string | null, successCriteria: Array<string>, constraints: Array<string>, raw: string } };

export type CreateRequirementMutationVariables = Exact<{
  title: Scalars['String']['input'];
  description?: InputMaybe<Scalars['String']['input']>;
  priority?: InputMaybe<Scalars['String']['input']>;
  requirementType?: InputMaybe<Scalars['String']['input']>;
  acceptanceCriteria?: InputMaybe<Array<Scalars['String']['input']> | Scalars['String']['input']>;
}>;


export type CreateRequirementMutation = { __typename?: 'MutationRoot', createRequirement: { __typename?: 'GqlRequirement', id: string } };

export type UpdateRequirementMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  title?: InputMaybe<Scalars['String']['input']>;
  description?: InputMaybe<Scalars['String']['input']>;
  priority?: InputMaybe<Scalars['String']['input']>;
  status?: InputMaybe<Scalars['String']['input']>;
  requirementType?: InputMaybe<Scalars['String']['input']>;
  acceptanceCriteria?: InputMaybe<Array<Scalars['String']['input']> | Scalars['String']['input']>;
}>;


export type UpdateRequirementMutation = { __typename?: 'MutationRoot', updateRequirement: { __typename?: 'GqlRequirement', id: string } };

export type DeleteRequirementMutationVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type DeleteRequirementMutation = { __typename?: 'MutationRoot', deleteRequirement: boolean };

export type DraftRequirementMutationVariables = Exact<{
  context?: InputMaybe<Scalars['String']['input']>;
}>;


export type DraftRequirementMutation = { __typename?: 'MutationRoot', draftRequirement: { __typename?: 'GqlRequirement', id: string, title: string } };

export type RefineRequirementMutationVariables = Exact<{
  id: Scalars['String']['input'];
  feedback?: InputMaybe<Scalars['String']['input']>;
}>;


export type RefineRequirementMutation = { __typename?: 'MutationRoot', refineRequirement: { __typename?: 'GqlRequirement', id: string } };

export type ProjectsQueryVariables = Exact<{ [key: string]: never; }>;


export type ProjectsQuery = { __typename?: 'QueryRoot', projects: Array<{ __typename?: 'GqlProject', id: string, name?: string | null, path?: string | null, description?: string | null, archived: boolean }>, projectsActive: Array<{ __typename?: 'GqlProject', id: string, name?: string | null, path?: string | null }> };

export type ProjectDetailQueryVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type ProjectDetailQuery = { __typename?: 'QueryRoot', project?: { __typename?: 'GqlProject', id: string, name?: string | null, path?: string | null, description?: string | null, type?: string | null, techStack: Array<string>, archived: boolean } | null };

export type CreateProjectMutationVariables = Exact<{
  name: Scalars['String']['input'];
  path: Scalars['String']['input'];
  description?: InputMaybe<Scalars['String']['input']>;
  projectType?: InputMaybe<Scalars['String']['input']>;
}>;


export type CreateProjectMutation = { __typename?: 'MutationRoot', createProject: { __typename?: 'GqlProject', id: string, name?: string | null } };

export type UpdateProjectMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  name?: InputMaybe<Scalars['String']['input']>;
  description?: InputMaybe<Scalars['String']['input']>;
  projectType?: InputMaybe<Scalars['String']['input']>;
}>;


export type UpdateProjectMutation = { __typename?: 'MutationRoot', updateProject: { __typename?: 'GqlProject', id: string, name?: string | null } };

export type DeleteProjectMutationVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type DeleteProjectMutation = { __typename?: 'MutationRoot', deleteProject: boolean };

export type LoadProjectMutationVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type LoadProjectMutation = { __typename?: 'MutationRoot', loadProject: { __typename?: 'GqlProject', id: string, name?: string | null } };

export type ArchiveProjectMutationVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type ArchiveProjectMutation = { __typename?: 'MutationRoot', archiveProject: { __typename?: 'GqlProject', id: string, name?: string | null, archived: boolean } };

export type QueueQueryVariables = Exact<{ [key: string]: never; }>;


export type QueueQuery = { __typename?: 'QueryRoot', queue: Array<{ __typename?: 'GqlQueueEntry', taskId: string, title?: string | null, priority?: GqlPriority | null, status?: GqlTaskStatus | null, waitTime?: number | null, position?: number | null }>, queueStats: { __typename?: 'GqlQueueStats', depth: number, readyCount: number, heldCount: number, avgWait?: number | null, throughput?: number | null } };

export type QueueHoldMutationVariables = Exact<{
  taskId: Scalars['String']['input'];
  reason?: InputMaybe<Scalars['String']['input']>;
}>;


export type QueueHoldMutation = { __typename?: 'MutationRoot', queueHold: boolean };

export type QueueReleaseMutationVariables = Exact<{
  taskId: Scalars['String']['input'];
}>;


export type QueueReleaseMutation = { __typename?: 'MutationRoot', queueRelease: boolean };

export type QueueReorderMutationVariables = Exact<{
  taskIds: Array<Scalars['String']['input']> | Scalars['String']['input'];
}>;


export type QueueReorderMutation = { __typename?: 'MutationRoot', queueReorder: boolean };

export type ReviewHandoffMutationVariables = Exact<{
  targetRole: Scalars['String']['input'];
  question: Scalars['String']['input'];
  context?: InputMaybe<Scalars['String']['input']>;
}>;


export type ReviewHandoffMutation = { __typename?: 'MutationRoot', reviewHandoff: boolean };

export type WorkflowConfigQueryVariables = Exact<{ [key: string]: never; }>;


export type WorkflowConfigQuery = { __typename?: 'QueryRoot', workflowConfig: { __typename?: 'GqlWorkflowConfig', mcpServers: Array<{ __typename?: 'GqlMcpServer', name: string, command: string, args: Array<string>, transport?: string | null, tools: Array<string>, env: Array<{ __typename?: 'GqlKeyValue', key: string, value: string }> }>, phaseCatalog: Array<{ __typename?: 'GqlPhaseCatalogEntry', id: string, label: string, description: string, category: string, tags: Array<string> }>, tools: Array<{ __typename?: 'GqlToolDefinition', name: string, executable: string, supportsMcp: boolean, supportsWrite: boolean, contextWindow?: number | null }>, agentProfiles: Array<{ __typename?: 'GqlAgentProfile', name: string, description: string, role?: string | null, mcpServers: Array<string>, skills: Array<string>, tool?: string | null, model?: string | null }>, schedules: Array<{ __typename?: 'GqlWorkflowSchedule', id: string, cron: string, workflowRef?: string | null, command?: string | null, enabled: boolean }> } };

export type TasksQueryVariables = Exact<{
  status?: InputMaybe<Scalars['String']['input']>;
  search?: InputMaybe<Scalars['String']['input']>;
}>;


export type TasksQuery = { __typename?: 'QueryRoot', tasks: Array<{ __typename?: 'GqlTask', id: string, title: string, status: GqlTaskStatus, statusRaw: string, priority: GqlPriority, priorityRaw: string, taskType: GqlTaskType, taskTypeRaw: string, tags: Array<string>, linkedRequirementIds: Array<string> }>, taskStats: { __typename?: 'GqlTaskStats', total: number, byStatus?: string | null, byPriority?: string | null } };

export type TasksPrioritizedQueryVariables = Exact<{ [key: string]: never; }>;


export type TasksPrioritizedQuery = { __typename?: 'QueryRoot', tasksPrioritized: Array<{ __typename?: 'GqlTask', id: string, title: string, status: GqlTaskStatus, statusRaw: string, priority: GqlPriority, priorityRaw: string, taskType: GqlTaskType, taskTypeRaw: string, tags: Array<string> }> };

export type TaskDetailQueryVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type TaskDetailQuery = { __typename?: 'QueryRoot', task?: { __typename?: 'GqlTask', id: string, title: string, description: string, status: GqlTaskStatus, statusRaw: string, priority: GqlPriority, priorityRaw: string, taskType: GqlTaskType, taskTypeRaw: string, risk: GqlRiskLevel, scope: GqlScope, complexity: GqlComplexity, tags: Array<string>, linkedRequirementIds: Array<string>, checklist: Array<{ __typename?: 'GqlChecklist', id: string, description: string, completed: boolean }>, dependencies: Array<{ __typename?: 'GqlDependency', taskId: string, type: string }> } | null };

export type UpdateTaskStatusMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  status: Scalars['String']['input'];
}>;


export type UpdateTaskStatusMutation = { __typename?: 'MutationRoot', updateTaskStatus: { __typename?: 'GqlTask', id: string, status: GqlTaskStatus, statusRaw: string } };

export type CreateTaskMutationVariables = Exact<{
  title: Scalars['String']['input'];
  description?: InputMaybe<Scalars['String']['input']>;
  taskType?: InputMaybe<Scalars['String']['input']>;
  priority?: InputMaybe<Scalars['String']['input']>;
}>;


export type CreateTaskMutation = { __typename?: 'MutationRoot', createTask: { __typename?: 'GqlTask', id: string, title: string, status: GqlTaskStatus, statusRaw: string } };

export type UpdateTaskMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  title?: InputMaybe<Scalars['String']['input']>;
  description?: InputMaybe<Scalars['String']['input']>;
  taskType?: InputMaybe<Scalars['String']['input']>;
  priority?: InputMaybe<Scalars['String']['input']>;
  risk?: InputMaybe<Scalars['String']['input']>;
  scope?: InputMaybe<Scalars['String']['input']>;
  complexity?: InputMaybe<Scalars['String']['input']>;
}>;


export type UpdateTaskMutation = { __typename?: 'MutationRoot', updateTask: { __typename?: 'GqlTask', id: string, title: string, status: GqlTaskStatus, statusRaw: string } };

export type DeleteTaskMutationVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type DeleteTaskMutation = { __typename?: 'MutationRoot', deleteTask: boolean };

export type AssignAgentMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  role?: InputMaybe<Scalars['String']['input']>;
  model?: InputMaybe<Scalars['String']['input']>;
}>;


export type AssignAgentMutation = { __typename?: 'MutationRoot', assignAgent: { __typename?: 'GqlTask', id: string } };

export type AssignHumanMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  name: Scalars['String']['input'];
}>;


export type AssignHumanMutation = { __typename?: 'MutationRoot', assignHuman: { __typename?: 'GqlTask', id: string } };

export type ChecklistAddMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  description: Scalars['String']['input'];
}>;


export type ChecklistAddMutation = { __typename?: 'MutationRoot', checklistAdd: { __typename?: 'GqlTask', id: string, checklist: Array<{ __typename?: 'GqlChecklist', id: string, description: string, completed: boolean }> } };

export type ChecklistUpdateMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  itemId: Scalars['String']['input'];
  completed?: InputMaybe<Scalars['Boolean']['input']>;
  description?: InputMaybe<Scalars['String']['input']>;
}>;


export type ChecklistUpdateMutation = { __typename?: 'MutationRoot', checklistUpdate: { __typename?: 'GqlTask', id: string, checklist: Array<{ __typename?: 'GqlChecklist', id: string, description: string, completed: boolean }> } };

export type DependencyAddMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  dependsOn: Scalars['String']['input'];
  dependencyType?: InputMaybe<Scalars['String']['input']>;
}>;


export type DependencyAddMutation = { __typename?: 'MutationRoot', dependencyAdd: { __typename?: 'GqlTask', id: string, dependencies: Array<{ __typename?: 'GqlDependency', taskId: string, type: string }> } };

export type DependencyRemoveMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  dependsOn: Scalars['String']['input'];
}>;


export type DependencyRemoveMutation = { __typename?: 'MutationRoot', dependencyRemove: { __typename?: 'GqlTask', id: string, dependencies: Array<{ __typename?: 'GqlDependency', taskId: string, type: string }> } };

export type WorkflowsQueryVariables = Exact<{
  status?: InputMaybe<Scalars['String']['input']>;
}>;


export type WorkflowsQuery = { __typename?: 'QueryRoot', workflows: Array<{ __typename?: 'GqlWorkflow', id: string, taskId: string, workflowRef?: string | null, status: GqlWorkflowStatus, statusRaw: string, currentPhase?: string | null, totalReworks: number, phases: Array<{ __typename?: 'GqlPhaseExecution', phaseId: string, status: string, startedAt?: string | null, completedAt?: string | null, attempt: number, errorMessage?: string | null }> }> };

export type WorkflowDetailQueryVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type WorkflowDetailQuery = { __typename?: 'QueryRoot', workflow?: { __typename?: 'GqlWorkflow', id: string, taskId: string, workflowRef?: string | null, status: GqlWorkflowStatus, statusRaw: string, currentPhase?: string | null, totalReworks: number, phases: Array<{ __typename?: 'GqlPhaseExecution', phaseId: string, status: string, startedAt?: string | null, completedAt?: string | null, attempt: number, errorMessage?: string | null }>, decisions: Array<{ __typename?: 'GqlDecision', timestamp: string, phaseId: string, source: string, decision: string, targetPhase?: string | null, reason: string, confidence: number, risk: string }> } | null, workflowCheckpoints: Array<{ __typename?: 'GqlWorkflowCheckpoint', id: string, phase: string, timestamp?: string | null, data?: string | null }> };

export type RunWorkflowMutationVariables = Exact<{
  taskId: Scalars['String']['input'];
  workflowRef?: InputMaybe<Scalars['String']['input']>;
}>;


export type RunWorkflowMutation = { __typename?: 'MutationRoot', runWorkflow: { __typename?: 'GqlWorkflow', id: string, taskId: string, status: GqlWorkflowStatus, statusRaw: string } };

export type PauseWorkflowMutationVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type PauseWorkflowMutation = { __typename?: 'MutationRoot', pauseWorkflow: { __typename?: 'GqlWorkflow', id: string, status: GqlWorkflowStatus } };

export type ResumeWorkflowMutationVariables = Exact<{
  id: Scalars['ID']['input'];
  feedback?: InputMaybe<Scalars['String']['input']>;
}>;


export type ResumeWorkflowMutation = { __typename?: 'MutationRoot', resumeWorkflow: { __typename?: 'GqlWorkflow', id: string, status: GqlWorkflowStatus } };

export type CancelWorkflowMutationVariables = Exact<{
  id: Scalars['ID']['input'];
}>;


export type CancelWorkflowMutation = { __typename?: 'MutationRoot', cancelWorkflow: { __typename?: 'GqlWorkflow', id: string, status: GqlWorkflowStatus } };

export type ApprovePhaseMutationVariables = Exact<{
  workflowId: Scalars['ID']['input'];
  phaseId: Scalars['String']['input'];
  note?: InputMaybe<Scalars['String']['input']>;
}>;


export type ApprovePhaseMutation = { __typename?: 'MutationRoot', approvePhase: { __typename?: 'GqlWorkflow', id: string, status: GqlWorkflowStatus, statusRaw: string, currentPhase?: string | null } };

export class TypedDocumentString<TResult, TVariables>
  extends String
  implements DocumentTypeDecoration<TResult, TVariables>
{
  __apiType?: NonNullable<DocumentTypeDecoration<TResult, TVariables>['__apiType']>;
  private value: string;
  public __meta__?: Record<string, any> | undefined;

  constructor(value: string, __meta__?: Record<string, any> | undefined) {
    super(value);
    this.value = value;
    this.__meta__ = __meta__;
  }

  override toString(): string & DocumentTypeDecoration<TResult, TVariables> {
    return this.value;
  }
}

export const UpsertWorkflowDefinitionDocument = new TypedDocumentString(`
    mutation UpsertWorkflowDefinition($id: String!, $name: String!, $description: String, $phases: String!, $variables: String) {
  upsertWorkflowDefinition(
    id: $id
    name: $name
    description: $description
    phases: $phases
    variables: $variables
  )
}
    `) as unknown as TypedDocumentString<UpsertWorkflowDefinitionMutation, UpsertWorkflowDefinitionMutationVariables>;
export const DeleteWorkflowDefinitionDocument = new TypedDocumentString(`
    mutation DeleteWorkflowDefinition($id: ID!) {
  deleteWorkflowDefinition(id: $id)
}
    `) as unknown as TypedDocumentString<DeleteWorkflowDefinitionMutation, DeleteWorkflowDefinitionMutationVariables>;
export const DaemonDocument = new TypedDocumentString(`
    query Daemon {
  daemonStatus {
    healthy
    status
    statusRaw
    runnerConnected
    activeAgents
    maxAgents
    projectRoot
  }
  daemonHealth {
    healthy
    status
    runnerConnected
    runnerPid
    activeAgents
    daemonPid
  }
  agentRuns {
    runId
    taskId
    taskTitle
    workflowId
    phaseId
    status
  }
  daemonLogs(limit: 50) {
    timestamp
    level
    message
  }
}
    `) as unknown as TypedDocumentString<DaemonQuery, DaemonQueryVariables>;
export const DaemonStartDocument = new TypedDocumentString(`
    mutation DaemonStart {
  daemonStart
}
    `) as unknown as TypedDocumentString<DaemonStartMutation, DaemonStartMutationVariables>;
export const DaemonStopDocument = new TypedDocumentString(`
    mutation DaemonStop {
  daemonStop
}
    `) as unknown as TypedDocumentString<DaemonStopMutation, DaemonStopMutationVariables>;
export const DaemonPauseDocument = new TypedDocumentString(`
    mutation DaemonPause {
  daemonPause
}
    `) as unknown as TypedDocumentString<DaemonPauseMutation, DaemonPauseMutationVariables>;
export const DaemonResumeDocument = new TypedDocumentString(`
    mutation DaemonResume {
  daemonResume
}
    `) as unknown as TypedDocumentString<DaemonResumeMutation, DaemonResumeMutationVariables>;
export const DaemonClearLogsDocument = new TypedDocumentString(`
    mutation DaemonClearLogs {
  daemonClearLogs
}
    `) as unknown as TypedDocumentString<DaemonClearLogsMutation, DaemonClearLogsMutationVariables>;
export const DashboardDocument = new TypedDocumentString(`
    query Dashboard {
  taskStats {
    total
    byStatus
    byPriority
  }
  daemonHealth {
    healthy
    status
    runnerConnected
    activeDaemons: activeAgents
    daemonPid
  }
  agentRuns {
    runId
    taskId
    taskTitle
    workflowId
    phaseId
    status
  }
  systemInfo {
    platform
    version
    daemonStatus
    projectRoot
  }
  queueStats {
    depth
  }
}
    `) as unknown as TypedDocumentString<DashboardQuery, DashboardQueryVariables>;
export const ReadyTasksDocument = new TypedDocumentString(`
    query ReadyTasks($search: String, $limit: Int) {
  readyTasks(search: $search, limit: $limit) {
    id
    title
    statusRaw
    priorityRaw
    taskTypeRaw
  }
}
    `) as unknown as TypedDocumentString<ReadyTasksQuery, ReadyTasksQueryVariables>;
export const WorkflowDefinitionsDocument = new TypedDocumentString(`
    query WorkflowDefinitions {
  workflowDefinitions {
    id
    name
    description
    phases
  }
}
    `) as unknown as TypedDocumentString<WorkflowDefinitionsQuery, WorkflowDefinitionsQueryVariables>;
export const DispatchRequirementsDocument = new TypedDocumentString(`
    query DispatchRequirements {
  requirements {
    id
    title
    description
    priorityRaw
    statusRaw
    requirementType
    tags
    linkedTaskIds
    acceptanceCriteria
  }
}
    `) as unknown as TypedDocumentString<DispatchRequirementsQuery, DispatchRequirementsQueryVariables>;
export const DaemonEventsDocument = new TypedDocumentString(`
    subscription DaemonEvents($eventType: String) {
  daemonEvents(eventType: $eventType) {
    id
    seq
    timestamp
    eventType
    data
  }
}
    `) as unknown as TypedDocumentString<DaemonEventsSubscription, DaemonEventsSubscriptionVariables>;
export const VisionDocument = new TypedDocumentString(`
    query Vision {
  vision {
    title
    summary
    goals
    targetAudience
    successCriteria
    constraints
    raw
  }
}
    `) as unknown as TypedDocumentString<VisionQuery, VisionQueryVariables>;
export const RequirementsDocument = new TypedDocumentString(`
    query Requirements {
  requirements {
    id
    title
    description
    priority
    priorityRaw
    status
    statusRaw
    requirementType
    tags
    linkedTaskIds
    acceptanceCriteria
  }
}
    `) as unknown as TypedDocumentString<RequirementsQuery, RequirementsQueryVariables>;
export const RequirementDocument = new TypedDocumentString(`
    query Requirement($id: ID!) {
  requirement(id: $id) {
    id
    title
    description
    priority
    priorityRaw
    status
    statusRaw
    requirementType
    tags
    linkedTaskIds
    acceptanceCriteria
  }
}
    `) as unknown as TypedDocumentString<RequirementQuery, RequirementQueryVariables>;
export const RequirementDetailDocument = new TypedDocumentString(`
    query RequirementDetail($id: ID!) {
  requirement(id: $id) {
    id
    title
    description
    priority
    priorityRaw
    status
    statusRaw
    requirementType
    tags
    linkedTaskIds
    acceptanceCriteria
  }
}
    `) as unknown as TypedDocumentString<RequirementDetailQuery, RequirementDetailQueryVariables>;
export const SaveVisionDocument = new TypedDocumentString(`
    mutation SaveVision($content: String!) {
  saveVision(content: $content) {
    title
    summary
    goals
    targetAudience
    successCriteria
    constraints
    raw
  }
}
    `) as unknown as TypedDocumentString<SaveVisionMutation, SaveVisionMutationVariables>;
export const RefineVisionDocument = new TypedDocumentString(`
    mutation RefineVision($feedback: String) {
  refineVision(feedback: $feedback) {
    title
    summary
    goals
    targetAudience
    successCriteria
    constraints
    raw
  }
}
    `) as unknown as TypedDocumentString<RefineVisionMutation, RefineVisionMutationVariables>;
export const CreateRequirementDocument = new TypedDocumentString(`
    mutation CreateRequirement($title: String!, $description: String, $priority: String, $requirementType: String, $acceptanceCriteria: [String!]) {
  createRequirement(
    title: $title
    description: $description
    priority: $priority
    requirementType: $requirementType
    acceptanceCriteria: $acceptanceCriteria
  ) {
    id
  }
}
    `) as unknown as TypedDocumentString<CreateRequirementMutation, CreateRequirementMutationVariables>;
export const UpdateRequirementDocument = new TypedDocumentString(`
    mutation UpdateRequirement($id: ID!, $title: String, $description: String, $priority: String, $status: String, $requirementType: String, $acceptanceCriteria: [String!]) {
  updateRequirement(
    id: $id
    title: $title
    description: $description
    priority: $priority
    status: $status
    requirementType: $requirementType
    acceptanceCriteria: $acceptanceCriteria
  ) {
    id
  }
}
    `) as unknown as TypedDocumentString<UpdateRequirementMutation, UpdateRequirementMutationVariables>;
export const DeleteRequirementDocument = new TypedDocumentString(`
    mutation DeleteRequirement($id: ID!) {
  deleteRequirement(id: $id)
}
    `) as unknown as TypedDocumentString<DeleteRequirementMutation, DeleteRequirementMutationVariables>;
export const DraftRequirementDocument = new TypedDocumentString(`
    mutation DraftRequirement($context: String) {
  draftRequirement(context: $context) {
    id
    title
  }
}
    `) as unknown as TypedDocumentString<DraftRequirementMutation, DraftRequirementMutationVariables>;
export const RefineRequirementDocument = new TypedDocumentString(`
    mutation RefineRequirement($id: String!, $feedback: String) {
  refineRequirement(id: $id, feedback: $feedback) {
    id
  }
}
    `) as unknown as TypedDocumentString<RefineRequirementMutation, RefineRequirementMutationVariables>;
export const ProjectsDocument = new TypedDocumentString(`
    query Projects {
  projects {
    id
    name
    path
    description
    archived
  }
  projectsActive {
    id
    name
    path
  }
}
    `) as unknown as TypedDocumentString<ProjectsQuery, ProjectsQueryVariables>;
export const ProjectDetailDocument = new TypedDocumentString(`
    query ProjectDetail($id: ID!) {
  project(id: $id) {
    id
    name
    path
    description
    type
    techStack
    archived
  }
}
    `) as unknown as TypedDocumentString<ProjectDetailQuery, ProjectDetailQueryVariables>;
export const CreateProjectDocument = new TypedDocumentString(`
    mutation CreateProject($name: String!, $path: String!, $description: String, $projectType: String) {
  createProject(
    name: $name
    path: $path
    description: $description
    projectType: $projectType
  ) {
    id
    name
  }
}
    `) as unknown as TypedDocumentString<CreateProjectMutation, CreateProjectMutationVariables>;
export const UpdateProjectDocument = new TypedDocumentString(`
    mutation UpdateProject($id: ID!, $name: String, $description: String, $projectType: String) {
  updateProject(
    id: $id
    name: $name
    description: $description
    projectType: $projectType
  ) {
    id
    name
  }
}
    `) as unknown as TypedDocumentString<UpdateProjectMutation, UpdateProjectMutationVariables>;
export const DeleteProjectDocument = new TypedDocumentString(`
    mutation DeleteProject($id: ID!) {
  deleteProject(id: $id)
}
    `) as unknown as TypedDocumentString<DeleteProjectMutation, DeleteProjectMutationVariables>;
export const LoadProjectDocument = new TypedDocumentString(`
    mutation LoadProject($id: ID!) {
  loadProject(id: $id) {
    id
    name
  }
}
    `) as unknown as TypedDocumentString<LoadProjectMutation, LoadProjectMutationVariables>;
export const ArchiveProjectDocument = new TypedDocumentString(`
    mutation ArchiveProject($id: ID!) {
  archiveProject(id: $id) {
    id
    name
    archived
  }
}
    `) as unknown as TypedDocumentString<ArchiveProjectMutation, ArchiveProjectMutationVariables>;
export const QueueDocument = new TypedDocumentString(`
    query Queue {
  queue {
    taskId
    title
    priority
    status
    waitTime
    position
  }
  queueStats {
    depth
    readyCount
    heldCount
    avgWait
    throughput
  }
}
    `) as unknown as TypedDocumentString<QueueQuery, QueueQueryVariables>;
export const QueueHoldDocument = new TypedDocumentString(`
    mutation QueueHold($taskId: String!, $reason: String) {
  queueHold(taskId: $taskId, reason: $reason)
}
    `) as unknown as TypedDocumentString<QueueHoldMutation, QueueHoldMutationVariables>;
export const QueueReleaseDocument = new TypedDocumentString(`
    mutation QueueRelease($taskId: String!) {
  queueRelease(taskId: $taskId)
}
    `) as unknown as TypedDocumentString<QueueReleaseMutation, QueueReleaseMutationVariables>;
export const QueueReorderDocument = new TypedDocumentString(`
    mutation QueueReorder($taskIds: [String!]!) {
  queueReorder(taskIds: $taskIds)
}
    `) as unknown as TypedDocumentString<QueueReorderMutation, QueueReorderMutationVariables>;
export const ReviewHandoffDocument = new TypedDocumentString(`
    mutation ReviewHandoff($targetRole: String!, $question: String!, $context: String) {
  reviewHandoff(targetRole: $targetRole, question: $question, context: $context)
}
    `) as unknown as TypedDocumentString<ReviewHandoffMutation, ReviewHandoffMutationVariables>;
export const WorkflowConfigDocument = new TypedDocumentString(`
    query WorkflowConfig {
  workflowConfig {
    mcpServers {
      name
      command
      args
      transport
      tools
      env {
        key
        value
      }
    }
    phaseCatalog {
      id
      label
      description
      category
      tags
    }
    tools {
      name
      executable
      supportsMcp
      supportsWrite
      contextWindow
    }
    agentProfiles {
      name
      description
      role
      mcpServers
      skills
      tool
      model
    }
    schedules {
      id
      cron
      workflowRef
      command
      enabled
    }
  }
}
    `) as unknown as TypedDocumentString<WorkflowConfigQuery, WorkflowConfigQueryVariables>;
export const TasksDocument = new TypedDocumentString(`
    query Tasks($status: String, $search: String) {
  tasks(status: $status, search: $search) {
    id
    title
    status
    statusRaw
    priority
    priorityRaw
    taskType
    taskTypeRaw
    tags
    linkedRequirementIds
  }
  taskStats {
    total
    byStatus
    byPriority
  }
}
    `) as unknown as TypedDocumentString<TasksQuery, TasksQueryVariables>;
export const TasksPrioritizedDocument = new TypedDocumentString(`
    query TasksPrioritized {
  tasksPrioritized {
    id
    title
    status
    statusRaw
    priority
    priorityRaw
    taskType
    taskTypeRaw
    tags
  }
}
    `) as unknown as TypedDocumentString<TasksPrioritizedQuery, TasksPrioritizedQueryVariables>;
export const TaskDetailDocument = new TypedDocumentString(`
    query TaskDetail($id: ID!) {
  task(id: $id) {
    id
    title
    description
    status
    statusRaw
    priority
    priorityRaw
    taskType
    taskTypeRaw
    risk
    scope
    complexity
    tags
    linkedRequirementIds
    checklist {
      id
      description
      completed
    }
    dependencies {
      taskId
      type
    }
  }
}
    `) as unknown as TypedDocumentString<TaskDetailQuery, TaskDetailQueryVariables>;
export const UpdateTaskStatusDocument = new TypedDocumentString(`
    mutation UpdateTaskStatus($id: ID!, $status: String!) {
  updateTaskStatus(id: $id, status: $status) {
    id
    status
    statusRaw
  }
}
    `) as unknown as TypedDocumentString<UpdateTaskStatusMutation, UpdateTaskStatusMutationVariables>;
export const CreateTaskDocument = new TypedDocumentString(`
    mutation CreateTask($title: String!, $description: String, $taskType: String, $priority: String) {
  createTask(
    title: $title
    description: $description
    taskType: $taskType
    priority: $priority
  ) {
    id
    title
    status
    statusRaw
  }
}
    `) as unknown as TypedDocumentString<CreateTaskMutation, CreateTaskMutationVariables>;
export const UpdateTaskDocument = new TypedDocumentString(`
    mutation UpdateTask($id: ID!, $title: String, $description: String, $taskType: String, $priority: String, $risk: String, $scope: String, $complexity: String) {
  updateTask(
    id: $id
    title: $title
    description: $description
    taskType: $taskType
    priority: $priority
    risk: $risk
    scope: $scope
    complexity: $complexity
  ) {
    id
    title
    status
    statusRaw
  }
}
    `) as unknown as TypedDocumentString<UpdateTaskMutation, UpdateTaskMutationVariables>;
export const DeleteTaskDocument = new TypedDocumentString(`
    mutation DeleteTask($id: ID!) {
  deleteTask(id: $id)
}
    `) as unknown as TypedDocumentString<DeleteTaskMutation, DeleteTaskMutationVariables>;
export const AssignAgentDocument = new TypedDocumentString(`
    mutation AssignAgent($id: ID!, $role: String, $model: String) {
  assignAgent(id: $id, role: $role, model: $model) {
    id
  }
}
    `) as unknown as TypedDocumentString<AssignAgentMutation, AssignAgentMutationVariables>;
export const AssignHumanDocument = new TypedDocumentString(`
    mutation AssignHuman($id: ID!, $name: String!) {
  assignHuman(id: $id, name: $name) {
    id
  }
}
    `) as unknown as TypedDocumentString<AssignHumanMutation, AssignHumanMutationVariables>;
export const ChecklistAddDocument = new TypedDocumentString(`
    mutation ChecklistAdd($id: ID!, $description: String!) {
  checklistAdd(id: $id, description: $description) {
    id
    checklist {
      id
      description
      completed
    }
  }
}
    `) as unknown as TypedDocumentString<ChecklistAddMutation, ChecklistAddMutationVariables>;
export const ChecklistUpdateDocument = new TypedDocumentString(`
    mutation ChecklistUpdate($id: ID!, $itemId: String!, $completed: Boolean, $description: String) {
  checklistUpdate(
    id: $id
    itemId: $itemId
    completed: $completed
    description: $description
  ) {
    id
    checklist {
      id
      description
      completed
    }
  }
}
    `) as unknown as TypedDocumentString<ChecklistUpdateMutation, ChecklistUpdateMutationVariables>;
export const DependencyAddDocument = new TypedDocumentString(`
    mutation DependencyAdd($id: ID!, $dependsOn: String!, $dependencyType: String) {
  dependencyAdd(id: $id, dependsOn: $dependsOn, dependencyType: $dependencyType) {
    id
    dependencies {
      taskId
      type
    }
  }
}
    `) as unknown as TypedDocumentString<DependencyAddMutation, DependencyAddMutationVariables>;
export const DependencyRemoveDocument = new TypedDocumentString(`
    mutation DependencyRemove($id: ID!, $dependsOn: String!) {
  dependencyRemove(id: $id, dependsOn: $dependsOn) {
    id
    dependencies {
      taskId
      type
    }
  }
}
    `) as unknown as TypedDocumentString<DependencyRemoveMutation, DependencyRemoveMutationVariables>;
export const WorkflowsDocument = new TypedDocumentString(`
    query Workflows($status: String) {
  workflows(status: $status) {
    id
    taskId
    workflowRef
    status
    statusRaw
    currentPhase
    totalReworks
    phases {
      phaseId
      status
      startedAt
      completedAt
      attempt
      errorMessage
    }
  }
}
    `) as unknown as TypedDocumentString<WorkflowsQuery, WorkflowsQueryVariables>;
export const WorkflowDetailDocument = new TypedDocumentString(`
    query WorkflowDetail($id: ID!) {
  workflow(id: $id) {
    id
    taskId
    workflowRef
    status
    statusRaw
    currentPhase
    totalReworks
    phases {
      phaseId
      status
      startedAt
      completedAt
      attempt
      errorMessage
    }
    decisions {
      timestamp
      phaseId
      source
      decision
      targetPhase
      reason
      confidence
      risk
    }
  }
  workflowCheckpoints(workflowId: $id) {
    id
    phase
    timestamp
    data
  }
}
    `) as unknown as TypedDocumentString<WorkflowDetailQuery, WorkflowDetailQueryVariables>;
export const RunWorkflowDocument = new TypedDocumentString(`
    mutation RunWorkflow($taskId: String!, $workflowRef: String) {
  runWorkflow(taskId: $taskId, workflowRef: $workflowRef) {
    id
    taskId
    status
    statusRaw
  }
}
    `) as unknown as TypedDocumentString<RunWorkflowMutation, RunWorkflowMutationVariables>;
export const PauseWorkflowDocument = new TypedDocumentString(`
    mutation PauseWorkflow($id: ID!) {
  pauseWorkflow(id: $id) {
    id
    status
  }
}
    `) as unknown as TypedDocumentString<PauseWorkflowMutation, PauseWorkflowMutationVariables>;
export const ResumeWorkflowDocument = new TypedDocumentString(`
    mutation ResumeWorkflow($id: ID!, $feedback: String) {
  resumeWorkflow(id: $id, feedback: $feedback) {
    id
    status
  }
}
    `) as unknown as TypedDocumentString<ResumeWorkflowMutation, ResumeWorkflowMutationVariables>;
export const CancelWorkflowDocument = new TypedDocumentString(`
    mutation CancelWorkflow($id: ID!) {
  cancelWorkflow(id: $id) {
    id
    status
  }
}
    `) as unknown as TypedDocumentString<CancelWorkflowMutation, CancelWorkflowMutationVariables>;
export const ApprovePhaseDocument = new TypedDocumentString(`
    mutation ApprovePhase($workflowId: ID!, $phaseId: String!, $note: String) {
  approvePhase(workflowId: $workflowId, phaseId: $phaseId, note: $note) {
    id
    status
    statusRaw
    currentPhase
  }
}
    `) as unknown as TypedDocumentString<ApprovePhaseMutation, ApprovePhaseMutationVariables>;