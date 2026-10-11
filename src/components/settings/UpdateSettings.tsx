import { useEffect, useState } from 'react';
import { ArrowUpCircle, RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useUpdateStore } from '../../stores/useUpdateStore';
import { useConfigStore } from '../../stores/useConfigStore';
import { isTauri } from '../../utils/env';
import { getRunningVersion } from '../../services/updaterService';
import { version } from '../../../package.json';

const versionLabel = `${version}-codex/windows-session-switch`;

export default function UpdateSettings() {
    const { t } = useTranslation();
    const [runningVersion, setRunningVersion] = useState(versionLabel);
    useEffect(() => { if (isTauri()) void getRunningVersion().then(setRunningVersion).catch(() => {}); }, []);
    const { config, saveConfig, error: configError } = useConfigStore();
    const [startup, setStartup] = useState(config?.check_updates_on_startup !== false);
    useEffect(() => setStartup(config?.check_updates_on_startup !== false), [config?.check_updates_on_startup]);
    const { isChecking, updateInfo, error, checkForUpdates, setDialogOpen } = useUpdateStore();
    return <section className="rounded-2xl border border-gray-200/80 bg-white p-5 shadow-xs dark:border-slate-800 dark:bg-slate-900/80">
        <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="flex items-start gap-3"><ArrowUpCircle className="mt-1 h-5 w-5 text-blue-500" /><div><h3 className="text-base font-semibold">{t('updater.title')}</h3><p className="mt-1 text-xs text-gray-500">{t('updater.desc')}</p></div></div>
            <button type="button" disabled={isChecking || !isTauri()} onClick={() => void checkForUpdates()} className="inline-flex items-center gap-2 rounded-lg bg-blue-50 px-3 py-2 text-xs font-medium text-blue-600 transition-colors hover:bg-blue-100 disabled:opacity-50 dark:bg-blue-950 dark:text-blue-300"><RefreshCw size={14} className={isChecking ? 'animate-spin' : ''} />{t(isChecking ? 'updater.checking' : 'updater.check_now')}</button>
        </div>
        <div className="mt-4 flex flex-wrap items-center justify-between gap-3 text-xs">
            <span className="text-gray-500">{t('updater.current_version')} {updateInfo?.current_version || runningVersion}</span>
            <label className="flex cursor-pointer items-center gap-2"><input type="checkbox" disabled={!config} checked={startup} onChange={event => { if (!config) return; const next = event.target.checked; setStartup(next); void saveConfig({ ...config, check_updates_on_startup: next }, true).catch(() => setStartup(config.check_updates_on_startup !== false)); }} className="accent-blue-600" />{t('updater.startup')}</label>
        </div>
        {(error || configError) && <p role="alert" className="mt-3 text-xs text-red-600">{t('updater.failed')}</p>}
        {updateInfo && <p role="status" className="mt-3 text-xs">{updateInfo.has_update ? <button onClick={() => setDialogOpen(true)} className="text-blue-600 hover:underline">{t('updater.available', { version: updateInfo.latest_version })}</button> : t('updater.latest')}</p>}
    </section>;
}
