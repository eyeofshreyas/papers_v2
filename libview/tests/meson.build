cflags = [
  '-DG_LOG_DOMAIN="PapersViewTests"',
  '-DPAPERS_COMPILATION',
  '-DTESTDATADIR="@0@"'.format(join_paths(source_root, 'test-data'))
]

test_executable = executable(
  'test-pps-jobs',
  'test-pps-jobs.c',
  include_directories: [top_inc],
  dependencies: libppsview_dep,
  c_args: cflags
)

test('pps-jobs-test', test_executable, env: backend_env)

document_model_test_executable = executable(
  'test-pps-document-model',
  'test-pps-document-model.c',
  include_directories: [top_inc, context_inc],
  dependencies: libppsview_dep,
  c_args: cflags
)

test('pps-document-model-test', document_model_test_executable, env: backend_env)
