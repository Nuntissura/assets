import "./taskboard.css";
import { useEffect, useRef, useState, type ReactNode } from "react";

export type Task = {id: string; title: string; description: string; status: "todo" | "in_progress" | "done"; board_id?: string; revision?: number};
export type Board = {id: string; title: string};
export type Request = <T>(path: string, method?: string, body?: unknown) => Promise<T>;
export function Taskboard({scope, root, request, integration, targetTaskId}: {scope: string; root: string; request: Request; integration?: ReactNode; targetTaskId?: string|null}) {
  const [boards,setBoards]=useState<Board[]>([]), [selected,setSelected]=useState("default"), [tasks,setTasks]=useState<Task[]>([]);
  const [title,setTitle]=useState(""), [body,setBody]=useState(""), [boardTitle,setBoardTitle]=useState("");
  const [error,setError]=useState(""), [busy,setBusy]=useState(false), [editing,setEditing]=useState<Task|null>(null);
  const generation=useRef(0), live=useRef(false), mutation=useRef(false);
  const [retry,setRetry]=useState<{path:string;payload:unknown;kind:"task"|"board"}|null>(null);
  const target=targetTaskId;
  async function load(token:number) {
    const [catalogue,result]=await Promise.all([request<{boards:Board[]}>(root+"/taskboards"),request<{tasks:Task[]}>(root+"/tasks")]);
    if (!live.current || token!==generation.current) return;
    setBoards(catalogue.boards);setTasks(result.tasks);
    if (target) {const task=result.tasks.find(item=>item.id===target);if(task)setSelected(task.board_id||"default");}
  }
  useEffect(()=>{live.current=true;const token=++generation.current;mutation.current=false;setTasks([]);setBoards([]);setSelected("default");setTitle("");setBody("");setBoardTitle("");setEditing(null);setRetry(null);setError("");setBusy(false);void load(token).catch(e=>{if(live.current&&token===generation.current)setError(e.message);});return()=>{live.current=false;generation.current++;};},[scope,root,target]);
  async function run(action:()=>Promise<unknown>) {
    if (mutation.current) return;mutation.current=true;setBusy(true);setError("");const token=generation.current;
    try {await action();if(live.current&&token===generation.current)await load(token);}
    catch(e){if(live.current&&token===generation.current)setError((e as Error).message);}
    finally{if(live.current&&token===generation.current){mutation.current=false;setBusy(false);}}
  }
  async function create(attempt:NonNullable<typeof retry>) {
    const token=generation.current;setRetry(attempt);
    await request(attempt.path,"POST",attempt.payload);
    if(!live.current||generation.current!==token)return;
    setRetry(null);if(attempt.kind==="task"){setTitle("");setBody("");}else setBoardTitle("");
  }
  const visible=tasks.filter(task=>(task.board_id||"default")===selected);
  return <section className="taskboard" data-testid="taskboard" aria-busy={busy}>
    <header className="taskboard-header">
      <label>Taskboard<select data-testid="taskboard-selector" value={selected} disabled={busy} onChange={e=>{setSelected(e.target.value);setEditing(null);}}>{boards.map(board=><option key={board.id} value={board.id}>{board.title}</option>)}</select></label>
      <span>{visible.filter(task=>task.status==="done").length} of {visible.length} done</span>
      {integration}
      <form onSubmit={e=>{e.preventDefault();const attempt={path:root+"/taskboards",payload:{id:crypto.randomUUID(),title:boardTitle},kind:"board" as const};void run(()=>create(attempt));}}>
        <label>New board<input data-testid="new-board-title" value={boardTitle} maxLength={100} disabled={busy||!!retry} required onChange={e=>setBoardTitle(e.target.value)}/></label><button data-testid="create-board" disabled={busy||!!retry}>Create board</button>
      </form>
    </header>
    {error&&<div><p className="error" role="alert">{error}</p><button disabled={busy} onClick={()=>void run(async()=>{})}>Reload current tasks</button></div>}
    {retry&&<div role="status">The create request is retained. <button disabled={busy} data-testid="taskboard-retry" onClick={()=>void run(()=>create(retry))}>Retry same request</button></div>}
    <form className="panel taskboard-create-task" onSubmit={e=>{e.preventDefault();const attempt={path:root+"/tasks",payload:{id:crypto.randomUUID(),board_id:selected,title,description:body,status:"todo"},kind:"task" as const};void run(()=>create(attempt));}}>
      <label>Task header<input data-testid="task-title" value={title} maxLength={300} required disabled={busy||!!retry} onChange={e=>setTitle(e.target.value)}/></label>
      <label>Task body<textarea data-testid="task-body" value={body} maxLength={20000} disabled={busy||!!retry} onChange={e=>setBody(e.target.value)}/></label><button data-testid="add-task" disabled={busy||!!retry}>Add task</button>
    </form>
    <div className="board">{(["todo","in_progress","done"] as const).map((status,index)=><section className="board-column" data-testid={`column-${status}`} key={status}><h2>{["To do","In progress","Done"][index]}</h2>{visible.filter(task=>task.status===status).map(task=><article className="task-card" data-testid={`task-${task.id}`} key={task.id}>
      <strong>{task.title}</strong><p>{task.description}</p>
      <label>Status<select value={task.status} disabled={busy} onChange={e=>void run(()=>request(`${root}/tasks/${task.id}`,"PATCH",{status:e.target.value,expected_revision:task.revision||0}))}><option value="todo">To do</option><option value="in_progress">In progress</option><option value="done">Done</option></select></label>
      <button disabled={busy} onClick={()=>setEditing({...task})}>Edit</button>
      <button disabled={busy} onClick={()=>void run(()=>request(`${root}/tasks/${task.id}?expected_revision=${task.revision||0}`,"DELETE"))}>Delete</button>
      {editing?.id===task.id&&<form onSubmit={e=>{e.preventDefault();const update=editing,token=generation.current;void run(async()=>{await request(`${root}/tasks/${task.id}`,"PATCH",{title:update.title,description:update.description,board_id:update.board_id||"default",expected_revision:task.revision||0});if(live.current&&generation.current===token)setEditing(null);});}}>
        <label>Header<input aria-label="Edit task header" value={editing.title} required maxLength={300} disabled={busy} onChange={e=>setEditing({...editing,title:e.target.value})}/></label>
        <label>Body<textarea aria-label="Edit task body" value={editing.description} disabled={busy} maxLength={20000} onChange={e=>setEditing({...editing,description:e.target.value})}/></label>
        <label>Board<select aria-label="Move task to board" value={editing.board_id||"default"} disabled={busy} onChange={e=>setEditing({...editing,board_id:e.target.value})}>{boards.map(board=><option key={board.id} value={board.id}>{board.title}</option>)}</select></label>
        <button disabled={busy}>Save task</button><button type="button" disabled={busy} onClick={()=>setEditing(null)}>Cancel</button>
      </form>}
    </article>)}</section>)}</div>
  </section>;
}
