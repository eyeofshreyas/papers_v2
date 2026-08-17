#include <glib.h>
#include <pps-annotation-model.h>
#include <pps-document-model.h>
#include <pps-document.h>
#include <pps-init.h>
#include <pps-job.h>
#include <pps-jobs.h>

static PpsDocumentModel *
new_model (void)
{
	return g_object_new (PPS_TYPE_DOCUMENT_MODEL,
	                     "annotation-model", pps_annotation_model_new (),
	                     NULL);
}

static PpsDocument *
load_test_document (void)
{
	PpsJob *job = pps_job_load_new ();
	gchar *file_path = TESTDATADIR "/utf16le-annot.pdf";
	GFile *file = g_file_new_for_path (file_path);
	gchar *uri = g_file_get_uri (file);
	PpsDocument *document;

	pps_job_load_set_uri (PPS_JOB_LOAD (job), uri);
	pps_job_run (job);
	document = g_object_ref (pps_job_load_get_loaded_document (PPS_JOB_LOAD (job)));

	g_object_unref (file);
	g_free (uri);
	g_object_unref (job);

	return document;
}

static void
page_rotation_defaults_and_clears (void)
{
	PpsDocumentModel *model = new_model ();

	g_assert_cmpint (pps_document_model_get_page_rotation (model, 3), ==, 0);
	g_assert_cmpint (pps_document_model_get_effective_page_rotation (model, 3), ==, 0);

	pps_document_model_set_page_rotation (model, 3, 90);
	g_assert_cmpint (pps_document_model_get_page_rotation (model, 3), ==, 90);
	/* other pages are untouched */
	g_assert_cmpint (pps_document_model_get_page_rotation (model, 0), ==, 0);

	pps_document_model_set_page_rotation (model, 3, 0);
	g_assert_cmpint (pps_document_model_get_page_rotation (model, 3), ==, 0);

	g_object_unref (model);
}

static void
effective_rotation_wraps (void)
{
	PpsDocumentModel *model = new_model ();

	pps_document_model_set_rotation (model, 270);
	pps_document_model_set_page_rotation (model, 1, 180);
	g_assert_cmpint (pps_document_model_get_effective_page_rotation (model, 1), ==, 90);

	pps_document_model_set_rotation (model, 0);
	pps_document_model_set_page_rotation (model, 1, -90);
	g_assert_cmpint (pps_document_model_get_page_rotation (model, 1), ==, 270);
	g_assert_cmpint (pps_document_model_get_effective_page_rotation (model, 1), ==, 270);

	g_object_unref (model);
}

static void
set_document_clears_page_rotations (void)
{
	PpsDocumentModel *model = new_model ();
	PpsDocument *doc1 = load_test_document ();
	PpsDocument *doc2 = load_test_document ();

	pps_document_model_set_document (model, doc1);
	pps_document_model_set_page_rotation (model, 0, 90);
	g_assert_cmpint (pps_document_model_get_page_rotation (model, 0), ==, 90);

	pps_document_model_set_document (model, doc2);
	g_assert_cmpint (pps_document_model_get_page_rotation (model, 0), ==, 0);

	g_object_unref (model);
	g_object_unref (doc1);
	g_object_unref (doc2);
}

gint
main (gint argc,
      gchar *argv[])
{
	gboolean hasBackend;

	g_test_init (&argc, &argv, NULL);
	hasBackend = pps_init ();
	g_assert_true (hasBackend);

	g_test_add_func ("/libview-document-model/page_rotation_defaults_and_clears", page_rotation_defaults_and_clears);
	g_test_add_func ("/libview-document-model/effective_rotation_wraps", effective_rotation_wraps);
	g_test_add_func ("/libview-document-model/set_document_clears_page_rotations", set_document_clears_page_rotations);

	return g_test_run ();
}
