let projectData = null;
let currentSelection = { type: 'overview', id: null };
let currentSubTab = 'code';
let currentVarFilter = 'ALL';

window.addEventListener('DOMContentLoaded', () => {
  // Notify native host that webview is ready
  if (window.ipc) {
    window.ipc.postMessage(JSON.stringify({ event: 'init' }));
  } else if (window.__TAURI_POST_MESSAGE__) {
    window.__TAURI_POST_MESSAGE__(JSON.stringify({ event: 'init' }));
  }

  // Setup search
  const searchInput = document.getElementById('search-input');
  if (searchInput) {
    searchInput.addEventListener('input', (e) => {
      filterProject(e.target.value.trim().toLowerCase());
    });
  }

  // Setup export buttons
  const btnExportSt = document.getElementById('btn-export-st');
  if (btnExportSt) {
    btnExportSt.addEventListener('click', exportStructuredText);
  }
  const btnExportCsv = document.getElementById('btn-export-csv');
  if (btnExportCsv) {
    btnExportCsv.addEventListener('click', exportVariablesCsv);
  }
});

// Called by native host when document is loaded
window.loadProject = function(data) {
  projectData = typeof data === 'string' ? JSON.parse(data) : data;
  renderHeader();
  renderSidebar();
  selectItem('overview', null);
};

function renderHeader() {
  if (!projectData) return;
  document.getElementById('project-name').textContent = projectData.project_name || 'PLCnext Project';
  const target = projectData.manifest?.target || 'AXC F 2152';
  const fw = projectData.manifest?.firmware ? ` (FW: ${projectData.manifest.firmware})` : '';
  document.getElementById('target-badge').textContent = target + fw;
}

function renderSidebar() {
  const tree = document.getElementById('tree-content');
  if (!tree || !projectData) return;
  tree.innerHTML = '';

  // 1. Overview
  addTreeNode(tree, '📊', 'Project Overview', 'overview', null, null);

  // 2. ESM Tasks
  const taskCount = projectData.tasks?.length || 0;
  addTreeNode(tree, '⏱️', 'Tasks (ESM)', 'tasks', null, taskCount);

  // 3. Programs
  const programs = (projectData.pous || []).filter(p => p.pou_type === 'Program');
  if (programs.length > 0) {
    addTreeSection(tree, 'PROGRAMS', programs.length);
    programs.forEach(p => {
      addTreeNode(tree, '📦', p.name, 'pou', p.name, p.variables?.length ? `${p.variables.length} vars` : null);
    });
  }

  // 4. Function Blocks
  const fbs = (projectData.pous || []).filter(p => p.pou_type === 'FunctionBlock');
  if (fbs.length > 0) {
    addTreeSection(tree, 'FUNCTION BLOCKS', fbs.length);
    fbs.forEach(fb => {
      addTreeNode(tree, '🧩', fb.name, 'pou', fb.name, fb.variables?.length ? `${fb.variables.length} vars` : null);
    });
  }

  // 5. Functions
  const fcs = (projectData.pous || []).filter(p => p.pou_type === 'Function');
  if (fcs.length > 0) {
    addTreeSection(tree, 'FUNCTIONS', fcs.length);
    fcs.forEach(fc => {
      addTreeNode(tree, '⚡', fc.name, 'pou', fc.name, null);
    });
  }

  // 6. Global & Port Variables
  const gVarCount = projectData.global_variables?.length || 0;
  addTreeNode(tree, '🌐', 'Global & GDS Ports', 'globals', null, gVarCount);

  // 7. Data Types
  const dtCount = projectData.data_types?.length || 0;
  if (dtCount > 0) {
    addTreeNode(tree, '🗃️', 'Data Types (UDTs)', 'datatypes', null, dtCount);
  }

  // 8. Hardware & Axioline I/O
  const hwCount = projectData.hardware?.length || 0;
  addTreeNode(tree, '🔌', 'Axioline Hardware I/O', 'hardware', null, hwCount);

  // 9. Raw Project Archive Files
  const fileCount = projectData.files?.length || 0;
  addTreeNode(tree, '📄', 'Project Files Tree', 'files', null, fileCount);
}

function addTreeSection(parent, title, count) {
  const el = document.createElement('div');
  el.className = 'tree-header';
  el.innerHTML = `<span>${title}</span><span>${count}</span>`;
  parent.appendChild(el);
}

function addTreeNode(parent, icon, title, type, id, badge) {
  const node = document.createElement('div');
  node.className = 'tree-node';
  node.dataset.type = type;
  node.dataset.id = id || '';
  node.innerHTML = `
    <span class="node-icon">${icon}</span>
    <span class="node-title">${title}</span>
    ${badge ? `<span class="node-badge">${badge}</span>` : ''}
  `;
  node.addEventListener('click', () => {
    document.querySelectorAll('.tree-node').forEach(n => n.classList.remove('active'));
    node.classList.add('active');
    selectItem(type, id);
  });
  parent.appendChild(node);
}

function selectItem(type, id) {
  currentSelection = { type, id };
  const titleEl = document.getElementById('panel-title');
  const typeBadgeEl = document.getElementById('panel-type-badge');
  const subTabsEl = document.getElementById('sub-tabs');
  const viewBody = document.getElementById('view-body');

  subTabsEl.style.display = 'none';

  if (type === 'overview') {
    titleEl.textContent = 'Project Overview & Metadata';
    typeBadgeEl.textContent = 'METADATA';
    renderOverview(viewBody);
  } else if (type === 'tasks') {
    titleEl.textContent = 'Execution & Synchronization Manager (ESM) Tasks';
    typeBadgeEl.textContent = 'ESM SCHEDULER';
    renderTasks(viewBody);
  } else if (type === 'globals') {
    titleEl.textContent = 'Global Data Space (GDS) & Global Variables';
    typeBadgeEl.textContent = 'PORT VARIABLES';
    renderVariablesTable(viewBody, projectData.global_variables || [], true);
  } else if (type === 'datatypes') {
    titleEl.textContent = 'User Defined Data Types (STRUCT / ENUM)';
    typeBadgeEl.textContent = 'IEC TYPES';
    renderDataTypes(viewBody);
  } else if (type === 'hardware') {
    titleEl.textContent = 'Controller & Axioline F / Bus Topology';
    typeBadgeEl.textContent = 'HARDWARE';
    renderHardware(viewBody);
  } else if (type === 'files') {
    titleEl.textContent = 'PLCnext Solution File Archive';
    typeBadgeEl.textContent = 'ARCHIVE';
    renderFiles(viewBody);
  } else if (type === 'pou') {
    const pou = (projectData.pous || []).find(p => p.name === id);
    if (!pou) return;
    titleEl.textContent = pou.name;
    typeBadgeEl.textContent = `${pou.pou_type} • ${pou.language}`;
    subTabsEl.style.display = 'flex';
    setupSubTabs(pou);
    renderPouSubTab(pou, currentSubTab);
  }
}

function setupSubTabs(pou) {
  const tabs = document.querySelectorAll('.sub-tab');
  tabs.forEach(tab => {
    tab.classList.toggle('active', tab.dataset.tab === currentSubTab);
    tab.onclick = () => {
      tabs.forEach(t => t.classList.remove('active'));
      tab.classList.add('active');
      currentSubTab = tab.dataset.tab;
      renderPouSubTab(pou, currentSubTab);
    };
  });
}

function renderPouSubTab(pou, subTab) {
  const viewBody = document.getElementById('view-body');
  if (subTab === 'code') {
    renderCode(viewBody, pou.code || '// No code worksheet available');
  } else if (subTab === 'vars') {
    renderVariablesTable(viewBody, pou.variables || [], false);
  } else if (subTab === 'docs') {
    renderDocumentation(viewBody, pou);
  }
}

function renderOverview(container) {
  const m = projectData.manifest || {};
  const pous = projectData.pous || [];
  const varsCount = (projectData.global_variables?.length || 0) + pous.reduce((acc, p) => acc + (p.variables?.length || 0), 0);

  container.innerHTML = `
    <div class="card-grid">
      <div class="card">
        <div class="card-title">Controller Target</div>
        <div class="card-prop"><span class="card-prop-label">Hardware PLC</span><span class="card-prop-value">${m.target || 'AXC F 2152'}</span></div>
        <div class="card-prop"><span class="card-prop-label">Firmware</span><span class="card-prop-value">${m.firmware || '2024.0 LTS'}</span></div>
        <div class="card-prop"><span class="card-prop-label">Schema Version</span><span class="card-prop-value">${m.schema_version || '1.0'}</span></div>
        <div class="card-prop"><span class="card-prop-label">Solution GUID</span><span class="card-prop-value" style="font-size:11px">${m.guid || 'N/A'}</span></div>
      </div>
      <div class="card">
        <div class="card-title">Project Statistics</div>
        <div class="card-prop"><span class="card-prop-label">Total POUs</span><span class="card-prop-value">${pous.length}</span></div>
        <div class="card-prop"><span class="card-prop-label">ESM Tasks</span><span class="card-prop-value">${projectData.tasks?.length || 0}</span></div>
        <div class="card-prop"><span class="card-prop-label">Total Variables</span><span class="card-prop-value">${varsCount}</span></div>
        <div class="card-prop"><span class="card-prop-label">Axioline Modules</span><span class="card-prop-value">${projectData.hardware?.length || 0}</span></div>
        <div class="card-prop"><span class="card-prop-label">Archive Files</span><span class="card-prop-value">${projectData.files?.length || 0}</span></div>
      </div>
      <div class="card">
        <div class="card-title">Author & Engineering Info</div>
        <div class="card-prop"><span class="card-prop-label">Author / Company</span><span class="card-prop-value">${m.author || 'Phoenix Contact'}</span></div>
        <div class="card-prop"><span class="card-prop-label">Project File</span><span class="card-prop-value" style="font-size:11px">${projectData.file_path}</span></div>
        <div class="card-prop"><span class="card-prop-label">Read-Only Mode</span><span class="card-prop-value" style="color:#10b981">Active (Non-destructive inspection)</span></div>
      </div>
    </div>
  `;
}

function renderCode(container, codeText) {
  const lines = codeText.split('\n');
  const lineNumsHtml = lines.map((_, i) => `<div>${i + 1}</div>`).join('');
  const highlightedCode = highlightIEC(codeText);

  container.innerHTML = `
    <div class="code-wrapper">
      <div class="line-numbers">${lineNumsHtml}</div>
      <div class="code-content">${highlightedCode}</div>
    </div>
  `;
}

function highlightIEC(text) {
  const escape = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  const lines = text.split('\n');

  return lines.map(line => {
    let l = escape(line);

    // Comments (* ... *) or //
    if (l.trim().startsWith('//')) {
      return `<span class="comm">${l}</span>`;
    }
    if (l.trim().startsWith('(*') && l.trim().endsWith('*)')) {
      return `<span class="comm">${l}</span>`;
    }

    // Inline comment
    let comment = '';
    const inlineCommIdx = l.indexOf('//');
    if (inlineCommIdx !== -1) {
      comment = `<span class="comm">${l.substring(inlineCommIdx)}</span>`;
      l = l.substring(0, inlineCommIdx);
    }

    // Strings '...'
    l = l.replace(/'([^']*)'/g, '<span class="str">\'$1\'</span>');

    // Keywords
    const keywords = [
      'PROGRAM', 'END_PROGRAM', 'FUNCTION_BLOCK', 'END_FUNCTION_BLOCK', 'FUNCTION', 'END_FUNCTION',
      'VAR_INPUT', 'VAR_OUTPUT', 'VAR_IN_OUT', 'VAR_GLOBAL', 'VAR_EXTERNAL', 'VAR_TEMP', 'VAR', 'END_VAR',
      'IF', 'THEN', 'ELSIF', 'ELSE', 'END_IF', 'CASE', 'OF', 'END_CASE', 'FOR', 'TO', 'BY', 'DO', 'END_FOR',
      'WHILE', 'END_WHILE', 'REPEAT', 'UNTIL', 'END_REPEAT', 'RETURN', 'EXIT', 'TRUE', 'FALSE',
      'AND', 'OR', 'XOR', 'NOT', 'MOD', 'RETAIN', 'CONSTANT'
    ];
    const kwRegex = new RegExp(`\\b(${keywords.join('|')})\\b`, 'g');
    l = l.replace(kwRegex, '<span class="kw">$1</span>');

    // Data Types
    const types = [
      'BOOL', 'BYTE', 'WORD', 'DWORD', 'LWORD', 'SINT', 'INT', 'DINT', 'LINT',
      'USINT', 'UINT', 'UDINT', 'ULINT', 'REAL', 'LREAL', 'TIME', 'DATE', 'TOD', 'DT', 'STRING', 'WSTRING'
    ];
    const typeRegex = new RegExp(`\\b(${types.join('|')})\\b`, 'g');
    l = l.replace(typeRegex, '<span class="type">$1</span>');

    // Operators
    l = l.replace(/(:=|&lt;=&gt;|&lt;=|&gt;=|&lt;&gt;|\+|-|\*|\/|=)/g, '<span class="op">$1</span>');

    return l + comment;
  }).join('\n');
}

function renderVariablesTable(container, vars, isGlobal) {
  currentVarFilter = 'ALL';
  const scopes = ['ALL', ...new Set(vars.map(v => v.scope))];

  const renderTableRows = (filter) => {
    const filtered = filter === 'ALL' ? vars : vars.filter(v => v.scope === filter);
    if (filtered.length === 0) {
      return `<tr><td colspan="7" style="text-align:center;color:var(--text-dim);padding:24px">No variables declared in this scope</td></tr>`;
    }
    return filtered.map(v => `
      <tr>
        <td><span class="scope-badge ${getScopeClass(v.scope)}">${v.scope}</span></td>
        <td style="font-weight:600">${v.name}</td>
        <td><span class="type">${v.data_type}</span></td>
        <td>${v.initial_value || '<span style="color:var(--text-dim)">-</span>'}</td>
        <td>${v.is_port ? '<span class="port-badge">PORT (GDS)</span>' : '<span style="color:var(--text-dim)">No</span>'}</td>
        <td>${v.is_retain ? 'RETAIN' : (v.is_constant ? 'CONST' : '-')}</td>
        <td style="color:var(--text-muted)">${v.comment || ''}</td>
      </tr>
    `).join('');
  };

  container.innerHTML = `
    <div class="table-filter-bar">
      ${scopes.map(s => `<div class="filter-pill ${s === 'ALL' ? 'active' : ''}" data-scope="${s}">${s}</div>`).join('')}
    </div>
    <table class="custom-table">
      <thead>
        <tr>
          <th>Scope</th>
          <th>Name</th>
          <th>Data Type</th>
          <th>Initial Value</th>
          <th>Global Data Space</th>
          <th>Attributes</th>
          <th>Description</th>
        </tr>
      </thead>
      <tbody id="var-table-body">
        ${renderTableRows('ALL')}
      </tbody>
    </table>
  `;

  container.querySelectorAll('.filter-pill').forEach(pill => {
    pill.addEventListener('click', () => {
      container.querySelectorAll('.filter-pill').forEach(p => p.classList.remove('active'));
      pill.classList.add('active');
      document.getElementById('var-table-body').innerHTML = renderTableRows(pill.dataset.scope);
    });
  });
}

function getScopeClass(scope) {
  if (scope.includes('INPUT')) return 'scope-in';
  if (scope.includes('OUTPUT')) return 'scope-out';
  if (scope.includes('IN_OUT')) return 'scope-inout';
  if (scope.includes('GLOBAL')) return 'scope-global';
  return 'scope-local';
}

function renderTasks(container) {
  const tasks = projectData.tasks || [];
  container.innerHTML = `
    <div class="card-grid">
      ${tasks.map(t => `
        <div class="card">
          <div class="card-title">⏱️ ${t.name} <span class="panel-type-badge">${t.task_type}</span></div>
          <div class="card-prop"><span class="card-prop-label">Cycle Time</span><span class="card-prop-value" style="color:#38bdf8;font-weight:700">${t.cycle_time}</span></div>
          <div class="card-prop"><span class="card-prop-label">Priority</span><span class="card-prop-value">${t.priority}</span></div>
          <div class="card-prop"><span class="card-prop-label">Watchdog</span><span class="card-prop-value">${t.watchdog}</span></div>
          <div style="margin-top:10px">
            <div style="font-size:12px;font-weight:600;margin-bottom:6px;color:var(--text-muted)">Scheduled Program Instances:</div>
            ${(t.programs || []).map(p => `
              <div style="font-size:12px;background:var(--bg-tertiary);padding:4px 8px;border-radius:3px;margin-bottom:3px;display:flex;justify-content:space-between">
                <span>📦 ${p.program_name}</span>
                <span style="color:var(--text-dim)">#${p.execution_order}</span>
              </div>
            `).join('')}
          </div>
        </div>
      `).join('')}
    </div>
  `;
}

function renderHardware(container) {
  const modules = projectData.hardware || [];
  container.innerHTML = `
    <div style="margin-bottom:12px;font-size:13px;color:var(--text-muted)">
      Axioline F Bus & Onboard I/O Terminal Configuration:
    </div>
    <div class="rack-container">
      ${modules.map(m => `
        <div class="rack-module">
          <div class="rack-header">
            <span>SLOT ${m.slot}</span>
            <span>${m.channels?.length || 0} CH</span>
          </div>
          <div class="rack-body">
            <div class="rack-title">${m.name}</div>
            <div class="rack-cat">Art.-No: ${m.catalog_number || 'N/A'}</div>
            <div style="font-size:10px;color:var(--text-dim);margin-top:2px">${m.description || ''}</div>
            <div class="rack-channels">
              ${(m.channels || []).map(ch => `
                <div class="rack-ch">
                  <span>${ch.name} (${ch.direction})</span>
                  <span style="color:var(--accent-phoenix)">${ch.mapped_symbol || ch.data_type}</span>
                </div>
              `).join('')}
            </div>
          </div>
        </div>
      `).join('')}
    </div>
  `;
}

function renderDataTypes(container) {
  const types = projectData.data_types || [];
  container.innerHTML = `
    <div class="card-grid">
      ${types.map(dt => `
        <div class="card">
          <div class="card-title">🗃️ ${dt.name} <span class="panel-type-badge">${dt.kind}</span></div>
          ${dt.kind === 'Struct' ? `
            <table class="custom-table" style="margin-top:8px">
              <thead><tr><th>Field</th><th>Type</th><th>Comment</th></tr></thead>
              <tbody>
                ${dt.fields.map(f => `<tr><td>${f.name}</td><td><span class="type">${f.data_type}</span></td><td>${f.comment || ''}</td></tr>`).join('')}
              </tbody>
            </table>
          ` : `
            <div style="display:flex;flex-wrap:wrap;gap:4px;margin-top:8px">
              ${dt.enum_values.map(ev => `<span class="scope-badge scope-local">${ev}</span>`).join('')}
            </div>
          `}
        </div>
      `).join('')}
    </div>
  `;
}

function renderFiles(container) {
  const files = projectData.files || [];
  container.innerHTML = `
    <table class="custom-table">
      <thead>
        <tr>
          <th>Relative Path in Archive</th>
          <th>Aspect</th>
          <th>POU</th>
          <th>Size</th>
        </tr>
      </thead>
      <tbody>
        ${files.map(f => `
          <tr>
            <td style="font-family:monospace;font-size:12px">${f.path}</td>
            <td><span class="panel-type-badge">${f.aspect}</span></td>
            <td>${f.pou_name || '-'}</td>
            <td>${formatBytes(f.size_bytes)}</td>
          </tr>
        `).join('')}
      </tbody>
    </table>
  `;
}

function renderDocumentation(container, pou) {
  container.innerHTML = `
    <div class="card" style="max-width:800px">
      <div class="card-title">Documentation for ${pou.name}</div>
      <div style="font-size:13px;line-height:1.6;color:var(--text-main);white-space:pre-wrap;padding:12px">
        ${pou.description || 'No rich text documentation or comments supplied for this POU.'}
      </div>
    </div>
  `;
}

function filterProject(query) {
  const nodes = document.querySelectorAll('.tree-node');
  nodes.forEach(node => {
    const text = node.textContent.toLowerCase();
    node.style.display = (!query || text.includes(query)) ? 'flex' : 'none';
  });
}

function exportStructuredText() {
  if (!projectData) return;
  let code = `(* Exported from PLCnext Project: ${projectData.project_name} *)\n\n`;
  (projectData.pous || []).forEach(pou => {
    code += `(* ==========================================\n   POU: ${pou.name} [${pou.pou_type}]\n   ========================================== *)\n`;
    code += pou.code + '\n\n';
  });
  downloadBlob(code, `${projectData.project_name}_Export.st`, 'text/plain');
}

function exportVariablesCsv() {
  if (!projectData) return;
  let csv = 'POU,Scope,Name,DataType,InitialValue,IsPort,Retain,Constant,Comment\n';
  (projectData.global_variables || []).forEach(v => {
    csv += `GLOBAL,${v.scope},${v.name},${v.data_type},${v.initial_value || ''},${v.is_port},${v.is_retain},${v.is_constant},"${v.comment}"\n`;
  });
  (projectData.pous || []).forEach(pou => {
    (pou.variables || []).forEach(v => {
      csv += `${pou.name},${v.scope},${v.name},${v.data_type},${v.initial_value || ''},${v.is_port},${v.is_retain},${v.is_constant},"${v.comment}"\n`;
    });
  });
  downloadBlob(csv, `${projectData.project_name}_Variables.csv`, 'text/csv');
}

function downloadBlob(content, filename, mime) {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

function formatBytes(bytes) {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
}
