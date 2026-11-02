'use strict';
'require view';
'require form';
'require uci';
'require rpc';
'require poll';
'require fs';

/*
 * luci-app-fluxdown — 运行状态/服务开关/监听地址/数据目录/访问密钥配置 + Web 界面跳转。
 * 完整下载管理界面由 fluxdown-agent 自带的 React SPA 提供，
 * LuCI 侧只做服务管理，不重复实现 UI。
 *
 * 汉化：手打 ipk 没有 po2lmo 工具链，视图内置 zh 词典；
 * 若将来接入标准 .lmo，_() 命中翻译时优先使用。
 * 界面语言取 <html lang>（主题按 dispatcher.lang 输出，如 zh_Hans / zh-cn）；
 * L.env 不含语言字段，不能用来判定。
 */

var DESC = 'Blazing fast multi-protocol download manager (HTTP/FTP/BitTorrent/HLS). ' +
	'Manage downloads in the full web interface after the service is running.';
var TOKEN_HELP = 'Leave empty to set the key on first visit of the web interface. ' +
	'When filled in, it overrides the stored key on every start. ' +
	'8–128 printable ASCII characters, containing both letters and digits.';

var ZH = {
	'Service status': '服务状态',
	'Running': '运行中',
	'Not running': '未运行',
	'Checking…': '检测中…',
	'Service settings': '服务设置',
	'Enabled': '启用',
	'Start the FluxDown server on boot.': '开机时自动启动 FluxDown 服务。',
	'Listen address': '监听地址',
	'HTTP listen address in host:port form.': 'HTTP 监听地址，格式为 host:port。',
	'Data directory': '数据目录',
	'Where the task database and logs are stored.': '任务数据库与日志的存放位置。',
	'Access key': '访问密钥',
	'Access key must be 8–128 characters long.': '访问密钥长度须为 8–128 位。',
	'Access key may only contain printable ASCII characters without spaces.':
		'访问密钥只能包含可见 ASCII 字符，不能含空格。',
	'Access key must contain both letters and digits.': '访问密钥必须同时包含字母和数字。',
	'Current access key': '当前访问密钥',
	'Not set yet — open the web interface to set one.': '尚未设置——请打开 Web 界面进行设置。',
	'Unable to read the access key.': '无法读取访问密钥。',
	'Show': '显示',
	'Hide': '隐藏',
	'Copy': '复制',
	'Copied': '已复制',
	'Web interface': 'Web 界面',
	'Open FluxDown': '打开 FluxDown'
};
ZH[DESC] = '极速多协议下载管理器（HTTP/FTP/BitTorrent/HLS）。服务运行后，请在完整的 Web 界面中管理下载任务。';
ZH[TOKEN_HELP] = '留空则在首次访问 Web 界面时设置。填写后，每次启动都以此为准并覆盖已保存的密钥。' +
	'需为 8–128 位可见 ASCII 字符，且同时包含字母和数字。';

var IS_ZH = /^zh/i.test(document.documentElement.getAttribute('lang') || '');

function T(s) {
	var t = _(s);
	if (t !== s)
		return t;
	return IS_ZH ? (ZH[s] || s) : s;
}

var callServiceList = rpc.declare({
	object: 'service',
	method: 'list',
	params: [ 'name' ],
	expect: { '': {} }
});

function bindToPort(bind) {
	var m = /:(\d+)\s*$/.exec(bind || '');
	return m ? m[1] : '17800';
}

function webUrl() {
	var port = bindToPort(uci.get('fluxdown', 'main', 'bind'));
	return 'http://' + window.location.hostname + ':' + port + '/';
}

/* 返回运行中的 PID（number）、true（运行中但无 PID）或 false */
function parseRunning(res) {
	var svc = res && res.fluxdown, inst, k;
	if (!svc || !svc.instances)
		return false;
	for (k in svc.instances) {
		inst = svc.instances[k];
		if (inst && inst.running)
			return inst.pid || true;
	}
	return false;
}

function renderStatus(node, state) {
	node.textContent = '';
	if (state === null) {
		node.appendChild(E('em', {}, T('Checking…')));
		return;
	}
	if (state === false) {
		node.appendChild(E('span', { 'style': 'color:#c00;font-weight:bold' }, T('Not running')));
		return;
	}
	node.appendChild(E('span', { 'style': 'color:#080;font-weight:bold' }, T('Running')));
	if (typeof state === 'number')
		node.appendChild(document.createTextNode(' (PID ' + state + ')'));
	node.appendChild(document.createTextNode(' · '));
	node.appendChild(E('a', { 'href': webUrl(), 'target': '_blank', 'rel': 'noopener' }, webUrl()));
}

function validateToken(value) {
	var i, c, letter = false, digit = false;
	if (value === '' || value == null)
		return true;
	if (value.length < 8 || value.length > 128)
		return T('Access key must be 8–128 characters long.');
	for (i = 0; i < value.length; i++) {
		c = value.charCodeAt(i);
		if (c < 0x21 || c > 0x7e)
			return T('Access key may only contain printable ASCII characters without spaces.');
		if ((c >= 0x30 && c <= 0x39))
			digit = true;
		else if ((c >= 0x41 && c <= 0x5a) || (c >= 0x61 && c <= 0x7a))
			letter = true;
	}
	if (!letter || !digit)
		return T('Access key must contain both letters and digits.');
	return true;
}

function copyText(text) {
	var ta;
	if (navigator.clipboard && window.isSecureContext)
		return navigator.clipboard.writeText(text);
	return new Promise(function(resolve, reject) {
		ta = E('textarea', { 'style': 'position:fixed;left:-9999px' });
		ta.value = text;
		document.body.appendChild(ta);
		ta.select();
		try {
			if (document.execCommand('copy'))
				resolve();
			else
				reject(new Error('copy failed'));
		} catch (e) {
			reject(e);
		} finally {
			document.body.removeChild(ta);
		}
	});
}

function renderToken(node, token) {
	var shown = false, span, showBtn, copyBtn;
	node.textContent = '';
	if (token === null) {
		node.appendChild(E('em', {}, T('Unable to read the access key.')));
		return;
	}
	if (token === '') {
		node.appendChild(E('em', {}, T('Not set yet — open the web interface to set one.')));
		return;
	}
	span = E('code', {}, '••••••••');
	showBtn = E('button', {
		'class': 'cbi-button',
		'style': 'margin-left:.5em',
		'click': function(ev) {
			ev.preventDefault();
			shown = !shown;
			span.textContent = shown ? token : '••••••••';
			showBtn.textContent = shown ? T('Hide') : T('Show');
		}
	}, T('Show'));
	copyBtn = E('button', {
		'class': 'cbi-button',
		'style': 'margin-left:.5em',
		'click': function(ev) {
			ev.preventDefault();
			copyText(token).then(function() {
				copyBtn.textContent = T('Copied');
				window.setTimeout(function() { copyBtn.textContent = T('Copy'); }, 1500);
			}).catch(function() {
				shown = true;
				span.textContent = token;
				showBtn.textContent = T('Hide');
			});
		}
	}, T('Copy'));
	node.appendChild(span);
	node.appendChild(showBtn);
	node.appendChild(copyBtn);
}

function loadToken() {
	return fs.exec_direct('/usr/libexec/fluxdown/luci-token').then(function(out) {
		return String(out || '').trim();
	}).catch(function() {
		return null;
	});
}

return view.extend({
	load: function() {
		return Promise.all([
			uci.load('fluxdown'),
			callServiceList('fluxdown').then(parseRunning).catch(function() { return false; }),
			loadToken()
		]);
	},

	render: function(data) {
		var m, s, o,
		    statusNode = E('span'),
		    tokenNode = E('span');

		renderStatus(statusNode, data[1]);
		renderToken(tokenNode, data[2]);

		poll.add(function() {
			return callServiceList('fluxdown').then(parseRunning).catch(function() {
				return false;
			}).then(function(state) {
				renderStatus(statusNode, state);
			});
		}, 5);

		m = new form.Map('fluxdown', T('FluxDown'),
			T(DESC));

		s = m.section(form.NamedSection, 'main', 'fluxdown', T('Service settings'));

		o = s.option(form.DummyValue, '_status', T('Service status'));
		o.rawhtml = true;
		o.cfgvalue = function() { return ''; };
		o.renderWidget = function() { return E('div', {}, statusNode); };

		o = s.option(form.Flag, 'enabled', T('Enabled'),
			T('Start the FluxDown server on boot.'));
		o.rmempty = false;
		o.default = '1';

		o = s.option(form.Value, 'bind', T('Listen address'),
			T('HTTP listen address in host:port form.'));
		o.placeholder = '[::]:17800';
		o.rmempty = true;

		o = s.option(form.Value, 'data_dir', T('Data directory'),
			T('Where the task database and logs are stored.'));
		o.placeholder = '/etc/fluxdown';
		o.rmempty = true;

		o = s.option(form.Value, 'token', T('Access key'),
			T(TOKEN_HELP));
		o.password = true;
		o.rmempty = true;
		o.validate = function(section_id, value) { return validateToken(value); };

		o = s.option(form.DummyValue, '_current_token', T('Current access key'));
		o.cfgvalue = function() { return ''; };
		o.renderWidget = function() { return E('div', {}, tokenNode); };

		o = s.option(form.Button, '_webui', T('Web interface'));
		o.inputtitle = T('Open FluxDown');
		o.inputstyle = 'apply';
		o.onclick = function() {
			window.open(webUrl(), '_blank');
		};

		return m.render();
	}
});
