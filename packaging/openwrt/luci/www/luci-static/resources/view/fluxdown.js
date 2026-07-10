'use strict';
'require view';
'require form';
'require uci';

/*
 * luci-app-fluxdown — 服务开关/监听地址/数据目录配置 + Web 界面跳转。
 * 完整下载管理界面由 fluxdown-server 自带的 React SPA 提供，
 * LuCI 侧只做服务管理，不重复实现 UI。
 */

function bindToPort(bind) {
	var m = /:(\d+)\s*$/.exec(bind || '');
	return m ? m[1] : '17800';
}

return view.extend({
	render: function() {
		var m, s, o;

		m = new form.Map('fluxdown', _('FluxDown'),
			_('Blazing fast multi-protocol download manager (HTTP/FTP/BitTorrent/HLS). ' +
			  'Manage downloads in the full web interface after the service is running.'));

		s = m.section(form.NamedSection, 'main', 'fluxdown', _('Service settings'));

		o = s.option(form.Flag, 'enabled', _('Enabled'),
			_('Start the FluxDown server on boot.'));
		o.rmempty = false;
		o.default = '1';

		o = s.option(form.Value, 'bind', _('Listen address'),
			_('HTTP listen address in host:port form.'));
		o.placeholder = '0.0.0.0:17800';
		o.rmempty = true;

		o = s.option(form.Value, 'data_dir', _('Data directory'),
			_('Where the task database and logs are stored.'));
		o.placeholder = '/etc/fluxdown';
		o.rmempty = true;

		o = s.option(form.Button, '_webui', _('Web interface'));
		o.inputtitle = _('Open FluxDown');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var port = bindToPort(uci.get('fluxdown', 'main', 'bind'));
			window.open('http://' + window.location.hostname + ':' + port + '/', '_blank');
		};

		return m.render();
	}
});
