use super::*;

pub(super) fn cached(
    root: &Path,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<Option<Registration>, Problem> {
    let (mut document, _) = read_storage(root, Storage::Official, check)?;
    Ok(document.registrations.pop())
}

pub(super) fn overlay(
    mut user: Vec<Registration>,
    official: Result<Option<Registration>, Problem>,
) -> (Vec<Registration>, Option<Problem>) {
    match official {
        Ok(None) => (effective_sources(user), None),
        Ok(Some(official)) => {
            user.retain(|source| !source.is_official());
            user.insert(0, official);
            (user, None)
        }
        Err(_) => {
            user.retain(|source| !source.is_official());
            user.insert(0, official_source());
            (user, Some(OFFICIAL_STORAGE_PROBLEM))
        }
    }
}

fn effective(
    root: &Path,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<(Vec<Registration>, Option<Problem>), Problem> {
    let (user, problem) = match run_user(root, Request::List, check, || {
        Err(Problem("source listing cannot publish metadata"))
    }) {
        Ok(Outcome::List(registrations)) => (registrations, None),
        Ok(_) => return Err(Problem("unexpected user source listing outcome")),
        Err(problem) => (Vec::new(), Some(problem)),
    };
    let (registrations, official_problem) = overlay(user, cached(root, check));
    Ok((registrations, problem.or(official_problem)))
}

fn current(root: &Path, check: &impl Fn() -> Result<(), Problem>) -> Result<Registration, Problem> {
    if let Some(official) = cached(root, check)? {
        return Ok(official);
    }
    let (document, _) = read(root, check)?;
    Ok(document
        .registrations
        .into_iter()
        .find(Registration::is_official)
        .unwrap_or_else(official_source))
}

pub(crate) fn run(
    root: &Path,
    request: Request,
    check: impl Fn() -> Result<(), Problem>,
    begin_commit: impl FnOnce() -> Result<(), Problem>,
) -> Result<Outcome, Problem> {
    check()?;
    if matches!(&request,Request::Remove(expected)|Request::ApprovePublisher{expected,..} if expected.is_official())
    {
        return Err(Problem("the application-owned official source cannot be removed or assigned a different publisher"));
    }
    if matches!(&request, Request::EffectiveList) {
        let (registrations, problem) = effective(root, &check)?;
        check()?;
        return Ok(Outcome::EffectiveList {
            registrations,
            problem,
        });
    }
    if let Request::Cached(expected) = &request {
        if expected.is_official() {
            let current = current(root, &check)?;
            if &current != expected {
                return Err(Problem(
                    "source was removed or changed; reopen its current registration",
                ));
            }
            current.available()?;
            let snapshot = current.snapshots.first().ok_or(Problem(
                "no retained metadata snapshot; refresh this source when online",
            ))?;
            let inspection = snapshot.inspection()?;
            check()?;
            return Ok(Outcome::Cached {
                inspection: Box::new(inspection),
                retrieved_at: snapshot.retrieved_at,
                stale: now().map_or(true, |now| current.stale_at(now)),
            });
        }
    }
    let official_request = match &request {
        Request::Add(inspection) => inspection.repository_id == official::REPOSITORY_ID,
        Request::Refresh {
            expected,
            inspection,
        }
        | Request::ApprovePublisher {
            expected,
            inspection,
        } => expected.is_official() || inspection.repository_id == official::REPOSITORY_ID,
        _ => false,
    };
    if official_request {
        return write(root, request, &check, begin_commit);
    }
    run_user(root, request, check, begin_commit)
}

fn write(
    root: &Path,
    request: Request,
    check: &impl Fn() -> Result<(), Problem>,
    begin_commit: impl FnOnce() -> Result<(), Problem>,
) -> Result<Outcome, Problem> {
    let (inspection, expected) = match request {
        Request::Add(inspection) => (inspection, None),
        Request::Refresh {
            expected,
            inspection,
        } if expected.is_official() => (inspection, Some(expected)),
        _ => {
            return Err(Problem(
                "official source identity cannot be retargeted or reassigned",
            ))
        }
    };
    let validated = Inspection::from_release(
        inspection.origin(),
        inspection.release.clone(),
        inspection.catalogue_bytes.clone(),
    )?;
    if validated.catalogue != inspection.catalogue
        || validated.catalogue_revision != inspection.catalogue_revision
    {
        return Err(Problem("inspection and raw metadata disagree"));
    }
    let origin = inspection.origin();
    if origin.repository_id != official::REPOSITORY_ID {
        return Err(Problem("numeric source identity changed"));
    }
    official::validate(&origin)?;
    if inspection.retrieved_at == 0 || inspection.retrieved_at > i64::MAX as u64 {
        return Err(Problem("invalid metadata retrieval time"));
    }
    let directory = store::open_root(root)?;
    let lock = store::file(&directory, "sources.lock", false)?;
    lock_registry(&lock, check)?;
    clean_temporaries(&directory, check)?;
    let (mut stored, previous) = read_storage(root, Storage::Official, check)?;
    let (user, user_previous, problem) = match read(root, check) {
        Ok((document, bytes)) => (document.registrations, Some(bytes), None),
        Err(problem) => (Vec::new(), None, Some(problem)),
    };
    let retained = stored
        .registrations
        .pop()
        .or_else(|| user.iter().find(|source| source.is_official()).cloned());
    let existed = retained.is_some();
    let mut registration = retained.unwrap_or_else(official_source);
    if let Some(expected) = expected {
        if expected != registration {
            return Err(Problem(
                "source was removed or changed; stale operation was refused",
            ));
        }
    } else if existed {
        return Err(Problem(
            "repository is already registered; use its Refresh action",
        ));
    }
    if !existed || origin.compare(&registration.origin) == Change::PublisherChanged {
        registration.token = store::token();
        registration.snapshots.clear();
    }
    let snapshot = Snapshot {
        release: inspection.release.clone(),
        origin: origin.clone(),
        revision: inspection.catalogue_revision.clone(),
        retrieved_at: inspection.retrieved_at,
        sha256: inventory::checksum(&inspection.catalogue_bytes)?,
        catalogue: String::from_utf8(inspection.catalogue_bytes.clone())
            .map_err(|_| Problem("catalogue is not UTF-8"))?,
    };
    registration.origin = origin;
    registration.revision = store::token();
    registration.snapshots.retain(|old| {
        old.revision != snapshot.revision
            || old.sha256 != snapshot.sha256
            || old.origin != snapshot.origin
            || old.release != snapshot.release
    });
    let protected = (
        registration.origin.repository_id,
        snapshot.sha256.clone(),
        snapshot.retrieved_at,
    );
    registration.snapshots.insert(0, snapshot);
    registration.snapshots.truncate(MAX_SNAPSHOTS);
    stored.registrations.push(registration);
    let bytes = trim(
        &mut stored,
        Some((protected.0, protected.1.as_str(), protected.2)),
        write_limit(&directory, Storage::Official, previous.as_deref())?,
        check,
    )?;
    let (registrations, _) = overlay(user, Ok(stored.registrations.pop()));
    let durability = save_storage_with_confirmation(
        root,
        &directory,
        (Storage::Official, previous),
        &bytes,
        check,
        || {
            begin_commit()?;
            if let Some(user_previous) = user_previous {
                if read_bytes(root)? != user_previous {
                    return Err(Problem("user source registry changed externally; refusing stale official cache publication"));
                }
            }
            Ok(())
        },
        File::sync_all,
    )?;
    Ok(Outcome::Changed {
        registrations,
        durability,
        problem,
    })
}
