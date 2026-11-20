// IPTV Panel Tab 2 Remodel (完全复用玩辰插件 - 原生 Element UI 宿主级融合)
(function() {
  function applyIptvPatch() {
    // 1. Rename Tab 2 from "玩辰插件" to "IPTV插件"
    var tab = document.getElementById('tab-OWDplugin');
    if (tab) {
      for (var i = 0; i < tab.childNodes.length; i++) {
        var node = tab.childNodes[i];
        if (node.nodeType === 3 && node.nodeValue && (node.nodeValue.indexOf('玩辰') !== -1 || node.nodeValue.indexOf('OWD') !== -1)) {
          node.nodeValue = 'IPTV插件';
        }
      }
      if (tab.textContent.indexOf('玩辰') !== -1) {
        tab.textContent = 'IPTV插件';
      }
    }

    // 2. Remodel native Element UI alert box inside pane-OWDplugin
    var pane = document.getElementById('pane-OWDplugin');
    if (pane) {
      var alertEl = pane.querySelector('.el-alert');
      if (alertEl) {
        var link = alertEl.querySelector('a.text-blue, a.text-href');
        if (link) {
          var targetUrl = window.location.protocol + '//' + window.location.hostname + ':4022';
          if (link.getAttribute('href') !== targetUrl) {
            link.setAttribute('href', targetUrl);
          }
          if (link.textContent !== 'IPTV设置页面' && link.textContent !== 'IPTV插件') {
            link.textContent = 'IPTV设置页面';
          }
          if (!link.__iptv_fixed) {
            link.__iptv_fixed = true;
            link.addEventListener('click', function(e) {
              e.stopImmediatePropagation();
            }, true);
          }
        }
        var spans = alertEl.querySelectorAll('span');
        for (var j = 0; j < spans.length; j++) {
          if (spans[j].textContent && spans[j].textContent.indexOf('玩辰') !== -1) {
            spans[j].textContent = spans[j].textContent.replace(/玩辰/g, 'IPTV');
          }
        }
      }
    }
  }

  var scheduled = false;
  var obs = new MutationObserver(function() {
    if (!scheduled) {
      scheduled = true;
      requestAnimationFrame(function() {
        scheduled = false;
        applyIptvPatch();
      });
    }
  });

  obs.observe(document.documentElement, {
    childList: true,
    subtree: true,
    characterData: true
  });

  window.addEventListener('hashchange', applyIptvPatch);
  document.addEventListener('DOMContentLoaded', applyIptvPatch);
  applyIptvPatch();
})();
